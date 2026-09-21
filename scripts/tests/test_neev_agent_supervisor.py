"""Fake-transport tests for the seven-op supervisor dispatcher.

The transport/fence are exercised elsewhere; here the dispatcher is driven as a pure
`handle(request) -> response` over in-memory state with a fake LoginDriver and fake
credential volumes, so every op's effect + the login state machine are verified
without sockets or subprocesses. Every response is round-tripped through the DTO
codec (and correlated where the DTO requires) so the wire stays valid."""

import sys
import unittest
from dataclasses import replace
from pathlib import Path

OPERATIONS = Path(__file__).parents[1] / "operations"
sys.path.insert(0, str(OPERATIONS))

import neev_supervisor_dto as dto  # noqa: E402
import neev_agent_supervisor as sup  # noqa: E402

BOOT = dto.Id("3" * 32)
COORD = dto.Coordinator(dto.Counter("1"), dto.Id("1" * 32))
DEVICE_URL = "https://auth.openai.com/codex/device"
CLAUDE_URL = "https://claude.com/cai/oauth/authorize?code=abc&state=xy"
SESSION = "123e4567-e89b-42d3-a456-426614174000"


def scope(provider):
    return dto.Scope(dto.OwnerId(1), dto.OwnerId(2), provider)


def binding(provider, attempt="5" * 32, conn="4" * 32, op="6" * 32):
    return dto.LoginBinding(
        scope(provider), dto.Id(conn), dto.Counter("1"),
        dto.SessionId(SESSION), dto.Id(attempt), dto.Id(op),
    )


def ticket(provider, expires=2000000000, lease="7" * 32):
    return dto.LoginTicketDto(binding(provider), dto.Id(lease), dto.UnixSec(expires))


def request(body, seq="1", rid="a" * 32):
    return dto.Request(COORD, dto.Counter(seq), dto.Id(rid), body)


class FakeDriver(sup.LoginDriver):
    def __init__(self, provider):
        self.provider = provider
        self.phase = "starting"
        self.account_state = "supported"
        self.account_label = "Acct"
        self.started = False
        self.canceled = False
        self.released = False
        self.submitted = None
        self.logout_result = "confirmed"
        if provider is dto.Provider.CODEX:
            self.challenge = sup.Challenge(DEVICE_URL, 1999999999, user_code="ABCD-EFGH")
        else:
            self.challenge = sup.Challenge(CLAUDE_URL, 1999999999, user_code=None)

    def start(self):
        self.started = True
        self.phase = "awaiting_user"

    def poll(self):
        return sup.DriverState(
            self.phase,
            challenge=self.challenge if self.phase == "awaiting_user" else None,
            account_label=self.account_label,
            account_state=self.account_state,
            failure="provider_unavailable" if self.phase == "failed" else None,
        )

    def submit_code(self, code):
        self.submitted = code
        self.phase = "prepared"

    def cancel(self):
        self.canceled = True

    def logout(self):
        return self.logout_result

    def release(self):
        self.released = True


class FakeVolumes:
    def __init__(self, revoke_ok=True):
        self.created = []
        self.closed = []
        self.revoked = []
        self.revoke_ok = revoke_ok
        self._n = 0

    def create(self, store_scope):
        self._n += 1
        handle = format(self._n, "032x")
        self.created.append((store_scope, handle))
        return handle

    def close(self, store_scope, handle):
        self.closed.append((store_scope, handle))

    def revoke(self, store_scope, handle):
        if not self.revoke_ok:
            raise RuntimeError("durable revoke failed")
        self.revoked.append((store_scope, handle))


class FakeProbe(sup.AccountProbe):
    def __init__(self, result="supported", fail=False):
        self._result = result
        self._fail = fail

    def probe(self):
        if self._fail:
            raise RuntimeError("probe failed")
        return (self._result, "Acct") if self._result == "supported" else self._result


class SupervisorTestBase(unittest.TestCase):
    def setUp(self):
        self.volumes = FakeVolumes()
        self.drivers = {}
        self._ids = 0
        self.clock = [1000]
        self.probe_result = "supported"
        self.probe_fail = False

        def driver_for(provider, store_scope, handle):
            driver = FakeDriver(provider)
            self.drivers[handle] = driver
            return driver

        def probe_for(provider, store_scope, handle):
            return FakeProbe(self.probe_result, self.probe_fail)

        def mint_id():
            self._ids += 1
            return format(0xF0000000 + self._ids, "032x")

        self.driver_for = driver_for
        self.probe_for = probe_for
        self.mint_id = mint_id
        self.sup = sup.Supervisor(
            BOOT, self.volumes, driver_for,
            now=lambda: self.clock[0], mint_id=mint_id, probe_for=probe_for,
        )
        self.sup.recover()  # in-memory mode: no durable backend, just open the gate

    def ok(self, request):
        response = self.sup.handle(request)
        self.assertIs(response.outcome, dto.Outcome.OK, getattr(response, "error", None))
        # The response is a valid wire message and correlates with its request.
        self.assertEqual(dto.decode_response(dto.encode_response(response)), response)
        return response

    def err(self, request):
        response = self.sup.handle(request)
        self.assertIs(response.outcome, dto.Outcome.ERROR)
        self.assertEqual(dto.decode_response(dto.encode_response(response)), response)
        return response.error.code


class LoginFlowTests(SupervisorTestBase):
    def test_codex_device_login_through_settle_and_reconcile(self):
        start = request(dto.StartLoginRequest(ticket(dto.Provider.CODEX)))
        response = self.ok(start)
        self.assertIs(response.body.disposition, dto.Disposition.ACCEPTED)
        self.assertEqual(len(self.volumes.created), 1)
        handle = self.volumes.created[0][1]
        self.assertTrue(self.drivers[handle].started)

        # Idempotent replay (same request_id) does not create a second volume.
        self.assertIs(self.ok(start).body.disposition, dto.Disposition.REPLAYED)
        self.assertEqual(len(self.volumes.created), 1)

        observe = request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
        snap = self.ok(observe).body
        self.assertIs(snap.state, dto.LoginState.AWAITING_USER)
        self.assertIsInstance(snap.challenge, dto.DeviceChallenge)
        self.assertEqual(snap.challenge.user_code.value, "ABCD-EFGH")

        # The user completes in-browser; the driver reaches prepared.
        self.drivers[handle].phase = "prepared"
        prepared = self.ok(observe).body
        self.assertIs(prepared.state, dto.LoginState.PREPARED)
        self.assertEqual(prepared.storage_handle.value, handle)
        receipt = prepared.receipt_id.value

        witness = dto.CommittedLoginWitnessDto(
            dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
            dto.Id(handle), dto.UnixSec(1500),
        )
        settle = request(
            dto.SettleLoginRequest(dto.Id(receipt), witness), seq="3", rid="c" * 32
        )
        settled = self.ok(settle)
        self.assertEqual(settled.body.receipt_id.value, receipt)
        self.assertEqual(settled.body.storage_handle.value, handle)
        dto.validate_response(settle, settled, BOOT)

        # A committed connection shows up in the reconcile page.
        page = self.ok(request(dto.ReconcilePageRequest(None, None, 32), seq="4", rid="d" * 32))
        dto.validate_response(
            request(dto.ReconcilePageRequest(None, None, 32), seq="4", rid="d" * 32),
            page, BOOT,
        )
        phases = {entry.phase for entry in page.body.entries}
        self.assertIn(dto.RegistryPhase.COMMITTED, phases)

    def test_settle_releases_login_resources_and_preserves_credentials(self):
        # Committing a login must free its transient resources (slot proxy + scope run
        # lock, via driver.release) WITHOUT revoking/logging out — the creds must persist,
        # and the freed slot/lock are what let the health probe + next runtime run proceed
        # (their absence caused UNAVAILABLE + a ~120s "provider failed to answer").
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        handle = self.volumes.created[0][1]
        driver = self.drivers[handle]
        driver.phase = "prepared"
        observe = request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
        receipt = self.ok(observe).body.receipt_id.value
        witness = dto.CommittedLoginWitnessDto(
            dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
            dto.Id(handle), dto.UnixSec(1500),
        )
        self.ok(request(dto.SettleLoginRequest(dto.Id(receipt), witness), seq="3", rid="c" * 32))
        self.assertTrue(driver.released)             # transient resources freed on commit
        self.assertEqual(self.volumes.revoked, [])   # credentials PRESERVED (no revoke)
        self.assertEqual(self.volumes.closed, [])    # not closed/logged-out on commit
        # Idempotent: a replayed settle still reports COMMITTED and does not re-release.
        driver.released = False
        self.ok(request(dto.SettleLoginRequest(dto.Id(receipt), witness), seq="4", rid="d" * 32))
        self.assertFalse(driver.released)

    def test_start_conflict_and_observe_not_found(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        # Same key, DIFFERENT ticket (different lease), new request id -> conflict.
        conflicting = request(
            dto.StartLoginRequest(ticket(dto.Provider.CODEX, lease="e" * 32)),
            rid="b" * 32,
        )
        self.assertIs(self.err(conflicting), dto.ErrorCode.CONFLICT)
        # Observe an unknown attempt.
        other = request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX, attempt="9" * 32)))
        self.assertIs(self.err(other), dto.ErrorCode.NOT_FOUND)

    def test_claude_authorization_code_submit(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CLAUDE))))
        handle = self.volumes.created[0][1]
        observe = request(dto.ObserveLoginRequest(binding(dto.Provider.CLAUDE)), seq="2", rid="b" * 32)
        snap = self.ok(observe).body
        self.assertIsInstance(snap.challenge, dto.AuthorizationCodeChallenge)
        self.assertTrue(snap.challenge.verification_url.startswith("https://claude.com/cai/oauth/authorize"))

        submit = request(
            dto.SubmitCodeRequest(binding(dto.Provider.CLAUDE), dto.AuthorizationCode("code-123")),
            seq="3", rid="c" * 32,
        )
        result = self.ok(submit)
        self.assertIs(result.op(), dto.Op.SUBMIT_CODE)
        self.assertIs(result.body.state, dto.LoginState.PREPARED)
        self.assertEqual(self.drivers[handle].submitted, "code-123")

    def test_submit_code_rejected_for_codex(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        submit = request(
            dto.SubmitCodeRequest(binding(dto.Provider.CODEX), dto.AuthorizationCode("x")),
            seq="2", rid="b" * 32,
        )
        self.assertIs(self.err(submit), dto.ErrorCode.CONFLICT)

    def test_settle_before_prepared_is_conflict(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        handle = self.volumes.created[0][1]
        witness = dto.CommittedLoginWitnessDto(
            dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
            dto.Id(handle), dto.UnixSec(1500),
        )
        settle = request(dto.SettleLoginRequest(dto.Id("a" * 32), witness), seq="2", rid="b" * 32)
        self.assertIs(self.err(settle), dto.ErrorCode.CONFLICT)

    def test_expiry_turns_into_terminal_expired(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX, expires=2000))))
        self.clock[0] = 2001  # past the ticket's expiry
        observe = request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
        snap = self.ok(observe).body
        self.assertIs(snap.state, dto.LoginState.EXPIRED)


class ConnectionLifecycleTests(SupervisorTestBase):
    def _commit_codex(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        handle = self.volumes.created[0][1]
        self.drivers[handle].phase = "prepared"
        prepared = self.ok(
            request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
        ).body
        witness = dto.CommittedLoginWitnessDto(
            dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
            dto.Id(handle), dto.UnixSec(1500),
        )
        self.ok(request(
            dto.SettleLoginRequest(prepared.receipt_id, witness), seq="3", rid="c" * 32
        ))
        return handle

    def _connection_binding(self):
        b = binding(dto.Provider.CODEX)
        return dto.ConnectionBinding(b.scope, b.connection_id, b.generation)

    def test_check_connection_finished_and_unavailable(self):
        handle = self._commit_codex()
        check_ticket = dto.CheckTicketDto(
            self._connection_binding(), dto.SessionId(SESSION), dto.Id(handle),
            dto.Id("a" * 32), dto.UnixSec(2000000000),
        )
        req = request(dto.CheckConnectionRequest(check_ticket), seq="4", rid="d" * 32)
        response = self.ok(req)
        self.assertIs(response.body.state, dto.CheckState.FINISHED)
        self.assertIsInstance(response.body.account, dto.SupportedAccount)
        dto.validate_response(req, response, BOOT)

        missing = dto.CheckTicketDto(
            self._connection_binding(), dto.SessionId(SESSION), dto.Id("f" * 32),
            dto.Id("b" * 32), dto.UnixSec(2000000000),
        )
        req2 = request(dto.CheckConnectionRequest(missing), seq="5", rid="e" * 32)
        response2 = self.ok(req2)
        self.assertIs(response2.body.state, dto.CheckState.UNAVAILABLE)
        dto.validate_response(req2, response2, BOOT)

    def test_cleanup_disconnect_logs_out_and_closes_volume(self):
        handle = self._commit_codex()
        cleanup_ticket = dto.CleanupTicketDto(
            scope(dto.Provider.CODEX), dto.Id("4" * 32), dto.Id("7" * 32),
            dto.CleanupTarget(dto.CleanupKind.DISCONNECT, None),
            dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
        )
        response = self.ok(request(
            dto.CleanupConnectionRequest(cleanup_ticket), seq="4", rid="d" * 32
        ))
        self.assertIs(response.body.state, dto.CleanupState.COMPLETED)
        self.assertIs(response.body.provider_logout, dto.LogoutResult.CONFIRMED)
        # Disconnect DURABLY revokes (deletes) the credential volume, not just close.
        self.assertEqual(self.volumes.closed, [(self.volumes.created[0][0], handle)])
        self.assertEqual(self.volumes.revoked, [(self.volumes.created[0][0], handle)])

    def test_cleanup_cancel_closes_volume_and_disposes_entry(self):
        self.ok(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        handle = self.volumes.created[0][1]
        cleanup_ticket = dto.CleanupTicketDto(
            scope(dto.Provider.CODEX), dto.Id("4" * 32), dto.Id("7" * 32),
            dto.CleanupTarget(dto.CleanupKind.CANCEL, dto.Id("5" * 32)),
            dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
        )
        response = self.ok(request(
            dto.CleanupConnectionRequest(cleanup_ticket), seq="2", rid="b" * 32
        ))
        self.assertIs(response.body.state, dto.CleanupState.COMPLETED)
        self.assertTrue(self.drivers[handle].canceled)
        self.assertEqual(self.volumes.closed, [(self.volumes.created[0][0], handle)])
        self.assertEqual(self.volumes.revoked, [(self.volumes.created[0][0], handle)])
        # The disposed entry surfaces in reconcile.
        page = self.ok(request(dto.ReconcilePageRequest(None, None, 32), seq="3", rid="c" * 32))
        self.assertIn(
            dto.RegistryPhase.DISPOSED, {entry.phase for entry in page.body.entries}
        )


class ReconcilePagingTests(SupervisorTestBase):
    def test_paging_cursor_and_stale_revision(self):
        # Start three attempts (distinct connections) -> three pending_login entries.
        for index in range(3):
            conn = str(index) * 32
            att = ("a" if index else "5") * 32 if index != 0 else "5" * 32
            b = binding(dto.Provider.CODEX, attempt=format(index + 1, "032x"), conn=conn)
            self.ok(request(dto.StartLoginRequest(
                dto.LoginTicketDto(b, dto.Id("7" * 32), dto.UnixSec(2000000000))
            ), seq=str(index + 1), rid=format(index + 10, "032x")))

        first_req = request(dto.ReconcilePageRequest(None, None, 2), seq="8", rid="d" * 32)
        first = self.ok(first_req)
        dto.validate_response(first_req, first, BOOT)
        self.assertEqual(len(first.body.entries), 2)
        self.assertIsNotNone(first.body.next_after_entry_id)
        revision = first.body.snapshot_revision

        cont_req = request(
            dto.ReconcilePageRequest(revision, first.body.next_after_entry_id, 2),
            seq="9", rid="e" * 32,
        )
        cont = self.ok(cont_req)
        dto.validate_response(cont_req, cont, BOOT)
        self.assertEqual(len(cont.body.entries), 1)
        self.assertIsNone(cont.body.next_after_entry_id)

        # A cursor against a superseded revision is refused stale.
        stale = request(
            dto.ReconcilePageRequest(dto.Counter(str(int(revision.value) + 999)),
                                     first.body.next_after_entry_id, 2),
            seq="10", rid="f" * 32,
        )
        self.assertIs(self.err(stale), dto.ErrorCode.STALE)


class _FakePending:
    def __init__(self, req):
        self.request = req


class _FakeSession:
    def __init__(self, pendings):
        self._pendings = list(pendings)
        self.responses = []
        self.closed = False

    async def receive(self):
        if self._pendings:
            return _FakePending(self._pendings.pop(0))
        raise sup.transport.TransportError("closed")

    async def respond(self, pending, response):
        self.responses.append(response)

    def close(self):
        self.closed = True


class _FakeFence:
    def __init__(self, register_error=None):
        self.registered = None
        self._register_error = register_error

    def register(self, coordinator):
        if self._register_error is not None:
            raise sup.fence_mod.FenceError(self._register_error)
        self.registered = coordinator
        return "registration"

    def admission(self, registration):
        import contextlib as _contextlib

        return _contextlib.nullcontext()


class ServeSessionTests(unittest.IsolatedAsyncioTestCase):
    def _supervisor(self):
        volumes = FakeVolumes()
        _drivers, driver_for = _drivers_sink()
        supervisor = sup.Supervisor(
            BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory()
        )
        supervisor.recover()
        return supervisor

    async def test_dispatches_under_admission_then_closes(self):
        supervisor = self._supervisor()
        start = request(dto.StartLoginRequest(ticket(dto.Provider.CODEX)))
        session = _FakeSession([start])
        fence = _FakeFence()
        await sup.serve_session(session, fence, supervisor)
        self.assertEqual(len(session.responses), 1)
        self.assertIs(session.responses[0].outcome, dto.Outcome.OK)
        self.assertEqual(fence.registered, start.coordinator)
        self.assertTrue(session.closed)

    async def test_fence_stale_registration_becomes_stale_error(self):
        supervisor = self._supervisor()
        start = request(dto.StartLoginRequest(ticket(dto.Provider.CODEX)))
        session = _FakeSession([start])
        await sup.serve_session(session, _FakeFence(register_error="Stale"), supervisor)
        self.assertEqual(len(session.responses), 1)
        self.assertIs(session.responses[0].outcome, dto.Outcome.ERROR)
        self.assertIs(session.responses[0].error.code, dto.ErrorCode.STALE)


def _mint_factory():
    counter = [0]

    def mint():
        counter[0] += 1
        return format(0xF0000000 + counter[0], "032x")

    return mint


def _drivers_sink():
    drivers = {}

    def driver_for(provider, store_scope, handle):
        driver = FakeDriver(provider)
        drivers[handle] = driver
        return driver

    return drivers, driver_for


class RecoveryAndFaultTests(unittest.TestCase):
    def _check_ticket(self, handle):
        b = binding(dto.Provider.CODEX)
        return dto.CheckTicketDto(
            dto.ConnectionBinding(b.scope, b.connection_id, b.generation),
            dto.SessionId(SESSION), dto.Id(handle), dto.Id("a" * 32), dto.UnixSec(2000000000),
        )

    def _disconnect_ticket(self):
        return dto.CleanupTicketDto(
            scope(dto.Provider.CODEX), dto.Id("4" * 32), dto.Id("7" * 32),
            dto.CleanupTarget(dto.CleanupKind.DISCONNECT, None),
            dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
        )

    def test_handle_is_refused_until_recovered(self):
        volumes = FakeVolumes()
        _drivers, driver_for = _drivers_sink()
        supervisor = sup.Supervisor(
            BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory()
        )
        start = request(dto.StartLoginRequest(ticket(dto.Provider.CODEX)))
        refused = supervisor.handle(start)
        self.assertIs(refused.outcome, dto.Outcome.ERROR)
        self.assertIs(refused.error.code, dto.ErrorCode.UNAVAILABLE)
        self.assertEqual(volumes.created, [])  # no effect before recovery
        supervisor.recover()
        self.assertIs(supervisor.handle(start).outcome, dto.Outcome.OK)

    def test_wrong_binding_same_ids_other_scope_is_not_found(self):
        volumes = FakeVolumes()
        _drivers, driver_for = _drivers_sink()
        supervisor = sup.Supervisor(
            BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory()
        )
        supervisor.recover()
        supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        # Same connection + attempt ids, DIFFERENT tenant scope -> must not resolve.
        impostor = dto.LoginBinding(
            dto.Scope(dto.OwnerId(9), dto.OwnerId(9), dto.Provider.CODEX),
            dto.Id("4" * 32), dto.Counter("1"), dto.SessionId(SESSION),
            dto.Id("5" * 32), dto.Id("6" * 32),
        )
        response = supervisor.handle(
            request(dto.ObserveLoginRequest(impostor), seq="2", rid="b" * 32)
        )
        self.assertIs(response.error.code, dto.ErrorCode.NOT_FOUND)

    def test_driver_start_failure_revokes_the_leaked_volume(self):
        volumes = FakeVolumes()

        def exploding_driver_for(provider, store_scope, handle):
            raise RuntimeError("driver init failed")

        supervisor = sup.Supervisor(
            BOOT, volumes, exploding_driver_for, now=lambda: 1000, mint_id=_mint_factory()
        )
        supervisor.recover()
        response = supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        self.assertIs(response.error.code, dto.ErrorCode.UNAVAILABLE)
        self.assertEqual(len(volumes.created), 1)
        self.assertEqual(len(volumes.revoked), 1)  # the half-provisioned volume was revoked

    def test_cleanup_revoke_failure_reports_failed_and_quarantines(self):
        volumes = FakeVolumes(revoke_ok=False)
        drivers, driver_for = _drivers_sink()
        supervisor = sup.Supervisor(
            BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory()
        )
        supervisor.recover()
        supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        cancel_ticket = dto.CleanupTicketDto(
            scope(dto.Provider.CODEX), dto.Id("4" * 32), dto.Id("7" * 32),
            dto.CleanupTarget(dto.CleanupKind.CANCEL, dto.Id("5" * 32)),
            dto.Counter("1"), dto.Counter("2"), dto.Id("8" * 32), dto.UnixSec(2000000000),
        )
        req = request(dto.CleanupConnectionRequest(cancel_ticket), seq="2", rid="b" * 32)
        failed = supervisor.handle(req)
        self.assertIs(failed.outcome, dto.Outcome.OK)
        self.assertIs(failed.body.state, dto.CleanupState.FAILED)  # no false COMPLETED
        # The attempt is quarantined (kept) so a later retry can finish it.
        page = supervisor.handle(request(dto.ReconcilePageRequest(None, None, 32), seq="3", rid="c" * 32))
        self.assertIn(dto.RegistryPhase.QUARANTINED, {e.phase for e in page.body.entries})
        # Once durable revoke works, the retry completes and disposes.
        volumes.revoke_ok = True
        retry = supervisor.handle(request(dto.CleanupConnectionRequest(cancel_ticket), seq="4", rid="d" * 32))
        self.assertIs(retry.body.state, dto.CleanupState.COMPLETED)

    def test_committed_connection_survives_restart_probe_and_disconnect(self):
        import os
        import tempfile

        probe_calls = []

        def probe_for(provider, store_scope, handle):
            probe_calls.append(handle)
            return FakeProbe("supported")

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "registry.json")
            volumes = FakeVolumes()
            drivers, driver_for = _drivers_sink()
            mint = _mint_factory()

            first = sup.Supervisor(
                BOOT, volumes, driver_for, now=lambda: 1000, mint_id=mint,
                probe_for=probe_for, durable=sup.FileRegistry(path),
            )
            first.recover()
            first.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
            handle = volumes.created[0][1]
            drivers[handle].phase = "prepared"
            observe = request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
            prepared = first.handle(observe).body
            witness = dto.CommittedLoginWitnessDto(
                dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
                dto.Id(handle), dto.UnixSec(1500),
            )
            first.handle(request(dto.SettleLoginRequest(prepared.receipt_id, witness), seq="3", rid="c" * 32))

            # Restart: a fresh supervisor over the SAME durable registry, no cached driver.
            second = sup.Supervisor(
                BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory(),
                probe_for=probe_for, durable=sup.FileRegistry(path),
            )
            second.recover()
            recovered = second.handle(observe).body
            self.assertIs(recovered.state, dto.LoginState.COMMITTED)
            self.assertEqual(recovered.storage_handle.value, handle)
            # A check post-restart runs an ACTUAL credential probe (no cached driver).
            check = second.handle(
                request(dto.CheckConnectionRequest(self._check_ticket(handle)), seq="4", rid="d" * 32)
            ).body
            self.assertIs(check.state, dto.CheckState.FINISHED)
            self.assertIsInstance(check.account, dto.SupportedAccount)
            self.assertEqual(probe_calls[-1], handle)  # probed THIS connection's volume
            # Disconnect after restart still DURABLY revokes the persisted volume.
            cleanup = second.handle(
                request(dto.CleanupConnectionRequest(self._disconnect_ticket()), seq="5", rid="e" * 32)
            ).body
            self.assertIs(cleanup.state, dto.CleanupState.COMPLETED)
            self.assertIn((volumes.created[0][0], handle), volumes.revoked)

    def test_check_connection_probe_failure_is_unavailable(self):
        volumes = FakeVolumes()
        drivers, driver_for = _drivers_sink()

        def failing_probe(provider, store_scope, handle):
            return FakeProbe(fail=True)

        supervisor = sup.Supervisor(
            BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory(),
            probe_for=failing_probe,
        )
        supervisor.recover()
        # Commit a connection.
        supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
        handle = volumes.created[0][1]
        drivers[handle].phase = "prepared"
        prepared = supervisor.handle(
            request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
        ).body
        witness = dto.CommittedLoginWitnessDto(
            dto.WitnessKind.COMPLETED_CONNECT, binding(dto.Provider.CODEX),
            dto.Id(handle), dto.UnixSec(1500),
        )
        supervisor.handle(request(dto.SettleLoginRequest(prepared.receipt_id, witness), seq="3", rid="c" * 32))
        b = binding(dto.Provider.CODEX)
        ticket_dto = dto.CheckTicketDto(
            dto.ConnectionBinding(b.scope, b.connection_id, b.generation),
            dto.SessionId(SESSION), dto.Id(handle), dto.Id("a" * 32), dto.UnixSec(2000000000),
        )
        check = supervisor.handle(
            request(dto.CheckConnectionRequest(ticket_dto), seq="4", rid="d" * 32)
        ).body
        self.assertIs(check.state, dto.CheckState.UNAVAILABLE)  # fail-closed, not faked

    def test_corrupt_registry_fails_closed_missing_is_clean(self):
        import os
        import tempfile

        # A PRESENT-but-corrupt registry aborts recovery (fail-closed) and the daemon
        # keeps refusing — never silently resets to empty (which would drop connections).
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "registry.json")
            with open(path, "w") as sink:
                sink.write("{ this is not valid json")
            volumes = FakeVolumes()
            _drivers, driver_for = _drivers_sink()
            supervisor = sup.Supervisor(
                BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory(),
                durable=sup.FileRegistry(path),
            )
            with self.assertRaises(sup.RegistryError):
                supervisor.recover()
            refused = supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))
            self.assertIs(refused.error.code, dto.ErrorCode.UNAVAILABLE)

        # A valid-JSON registry with a corrupt RECORD also aborts (not skipped).
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "registry.json")
            with open(path, "w") as sink:
                sink.write('{"e": {"entry_id": "not-32-hex", "phase": "committed"}}')
            supervisor = sup.Supervisor(
                BOOT, FakeVolumes(), _drivers_sink()[1], now=lambda: 1000,
                mint_id=_mint_factory(), durable=sup.FileRegistry(path),
            )
            with self.assertRaises(sup.RegistryError):
                supervisor.recover()

        # A MISSING registry is a clean first boot (empty, recovers fine).
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "absent.json")
            supervisor = sup.Supervisor(
                BOOT, FakeVolumes(), _drivers_sink()[1], now=lambda: 1000,
                mint_id=_mint_factory(), durable=sup.FileRegistry(path),
            )
            supervisor.recover()
            self.assertIs(
                supervisor.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX)))).outcome,
                dto.Outcome.OK,
            )

    def test_in_flight_login_recovers_as_interrupted(self):
        import os
        import tempfile

        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "registry.json")
            volumes = FakeVolumes()
            _drivers, driver_for = _drivers_sink()
            first = sup.Supervisor(
                BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory(),
                durable=sup.FileRegistry(path),
            )
            first.recover()
            first.handle(request(dto.StartLoginRequest(ticket(dto.Provider.CODEX))))

            second = sup.Supervisor(
                BOOT, volumes, driver_for, now=lambda: 1000, mint_id=_mint_factory(),
                durable=sup.FileRegistry(path),
            )
            second.recover()
            snap = second.handle(
                request(dto.ObserveLoginRequest(binding(dto.Provider.CODEX)), seq="2", rid="b" * 32)
            ).body
            self.assertIs(snap.state, dto.LoginState.INTERRUPTED)


if __name__ == "__main__":
    unittest.main()
