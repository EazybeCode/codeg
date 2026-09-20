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


class FakeVolumes:
    def __init__(self):
        self.created = []
        self.closed = []
        self._n = 0

    def create(self, store_scope):
        self._n += 1
        handle = format(self._n, "032x")
        self.created.append((store_scope, handle))
        return handle

    def close(self, store_scope, handle):
        self.closed.append((store_scope, handle))


class SupervisorTestBase(unittest.TestCase):
    def setUp(self):
        self.volumes = FakeVolumes()
        self.drivers = {}
        self._ids = 0
        self.clock = [1000]

        def driver_for(provider, store_scope, handle):
            driver = FakeDriver(provider)
            self.drivers[handle] = driver
            return driver

        def mint_id():
            self._ids += 1
            return format(0xF0000000 + self._ids, "032x")

        self.sup = sup.Supervisor(
            BOOT, self.volumes, driver_for,
            now=lambda: self.clock[0], mint_id=mint_id,
        )

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
        self.assertEqual(self.volumes.closed, [(self.volumes.created[0][0], handle)])

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


if __name__ == "__main__":
    unittest.main()
