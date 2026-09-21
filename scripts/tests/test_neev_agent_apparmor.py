"""Regression test for the scoped Neev AppArmor profile TEXT.

The profile must keep the narrow allowances the credential/egress/cancel paths need
AND must not regress into a blanket unconfine or a blanket mount/signal grant. Loading
and enforcing the profile is Linux+root (done on the host); this pins the policy text
so a careless edit is caught on any platform."""

import unittest
from pathlib import Path

PROFILE = Path(__file__).parents[1] / "operations" / "neev-agent.apparmor"


class ApparmorProfileTests(unittest.TestCase):
    def setUp(self):
        self.text = PROFILE.read_text()

    def test_allows_receiving_runtime_signals_so_docker_stop_works(self):
        # Without this the container cannot receive the runtime's SIGTERM, so `docker
        # stop` waits out the full termination grace before resorting to SIGKILL. The
        # audit shows the runtime confined as peer="runc", so that exact label must be
        # allowed; peer=unconfined is retained for hosts that run the runtime unconfined.
        self.assertIn("signal (receive) peer=runc,", self.text)
        self.assertIn("signal (receive) peer=unconfined,", self.text)
        # Intra-profile signalling (the launcher reaping its own children) stays.
        self.assertIn("signal (receive,send) peer=neev-agent,", self.text)

    def test_stays_narrow_not_a_blanket_unconfine_or_widened_signals(self):
        self.assertNotIn("(unconfined)", self.text)          # no unconfined flag
        self.assertNotIn("complain", self.text)              # enforce, never complain
        for blanket in ("\n  signal,\n", "\n  ptrace,\n", "\n  mount,\n"):
            self.assertNotIn(blanket, self.text)             # no bare signal/ptrace/mount
        # The container must not SEND signals to the unconfined runtime, only receive.
        self.assertNotIn("signal (send) peer=unconfined", self.text)
        self.assertNotIn("signal (receive,send) peer=unconfined", self.text)
        self.assertNotIn("signal peer=unconfined", self.text)

    def test_keeps_the_required_scoped_mounts(self):
        for required in (
            "mount fstype=ext4 options=(rw, nodev, nosuid, noexec)",
            "-> /data/agents/run-*/home/,",   # bind the volume onto the run's HOME
            "mount fstype=proc -> /proc/,",    # private /proc for the pid namespace
            "-> /run/netns/*,",                # per-slot netns management
            "/etc/netns/*/resolv.conf -> /etc/resolv.conf,",  # per-netns proxy DNS
        ):
            self.assertIn(required, self.text)


if __name__ == "__main__":
    unittest.main()
