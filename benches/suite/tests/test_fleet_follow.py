"""The gate's waiter (scripts/lib/fleet-job.sh, fleet_follow and fleet_results) through a fleet that
is silent for a while: silence is unknown, not an end. It is counted in seconds per spell, up to
NRESE_FLEET_SILENT_S and never past the gate's MAX_S; nothing is cancelled for it until the wait ends
there, with one cancel; a job that ended meanwhile is judged by its result once the fleet answers.

Run: python -m unittest discover -s benches/suite/tests -p test_fleet_follow.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import datetime
import json
import time
import unittest

import test_push_gate as push  # the scratch repository and the gate's job records (a module: its tests aren't rerun here)

READS = (".gitattributes", ".githooks/pre-push", "benches/fast/release_gate.py", "benches/suite/suitekit/",
         "benches/suite/tests/test_push_gate.py", "rust-toolchain.toml", "scripts/cargo-guarded.sh", "scripts/check.sh",
         "scripts/lib/*")

# A stand-in for the fleet's CLI whose status and results play $FAKE/<command>-<job>.seq, one reply
# a call, the last again once they are used up: "~N" waits N s and answers nothing (a call that
# times out), "+N TEXT" waits N s and then answers TEXT (a slow answer), "!TEXT" prints TEXT and
# fails (a CLI error on stdout), any other line is the answer.
SCRIPTED = """#!/bin/sh
case "$1" in
  submit) n=$(( $(cat "$FAKE/n" 2> /dev/null || echo 0) + 1 )); echo $n > "$FAKE/n"
          cp "$2" "$FAKE/job-$n.toml"; echo gate-$n ;;
  cancel) echo "$2" >> "$FAKE/cancelled" ;;
  status | results)
    seq="$FAKE/$1-$2.seq" calls="$FAKE/$1-$2.n"
    n=$(( $(cat "$calls" 2> /dev/null || echo 0) + 1 )); echo $n > "$calls"
    last=$(wc -l < "$seq"); [ "$n" -le "$last" ] || n=$last
    reply=$(sed -n "${n}p" "$seq")
    case $reply in
      "~"*) sleep "${reply#"~"}" ;;
      "+"*) wait=${reply#"+"}; sleep "${wait%% *}"; printf '%s\\n' "${reply#* }" ;;
      "!"*) echo "${reply#"!"}"; exit 1 ;;
      *) printf '%s\\n' "$reply" ;;
    esac ;;
esac
"""
# Each kind of silence: no reply, a CLI error, an answer without a state, a call that takes a second.
SILENCE = ['{"state": null}', "", "!error: the fleet's service is unreachable", '{"host": "office-pc"}', "~1"]
# Silence without the wait: six of these in a row are more than the old six-call limit allowed.
QUICK = SILENCE[1:4] * 2
FAILING = {"code": 1, "steps": [{"name": "test", "code": 1, "seconds": 1}]}


def state(name):
    return json.dumps({"state": name, "host": "office-pc"})


def pending(**fields):
    """A pending job's status, with FIELDS (expected_start, basis) as the fleet gives them."""
    return json.dumps({"state": "pending", "host": None, "reason": "office-pc: needs 8.0 cores, 2.0 free", **fields})


def expected(in_s):
    """The fleet's expected start IN_S seconds from now: its CLI's local time, without a zone."""
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(time.time() + in_s))


class FleetFollow(push.Repo):
    """scripts/lib/fleet-gate.sh's push tier for one commit, its fleet's answers scripted."""

    job = push.FleetGate.job
    gate = push.FleetGate.gate
    reset = push.FleetGate.reset

    def setUp(self):
        super().setUp()
        self.tip = self.commit("a.txt", "a changed\n")
        (self.fake / "fleet").write_text(SCRIPTED, newline="\n")
        self.env["NRESE_FLEET_SILENT_S"] = "30"

    def play(self, command, *replies):
        """Job gate-1's replies to COMMAND, in order."""
        (self.fake / f"{command}-gate-1.seq").write_text("".join(f"{r}\n" for r in replies), newline="\n")

    def results(self, **record):
        """Job gate-1's results: a whole, passing record of it, changed by RECORD; their directory."""
        self.job(1, "office-pc", "done", **record)
        return (self.fake / "results-gate-1").as_posix()

    def cancels(self):
        path = self.fake / "cancelled"
        return path.read_text().split() if path.exists() else []

    def calls(self, command):
        """How often job gate-1's COMMAND reached the fleet."""
        path = self.fake / f"{command}-gate-1.n"
        return int(path.read_text()) if path.exists() else 0

    def test_a_silence_shorter_than_the_cap_is_waited_out(self):
        """Silence for many more calls than the old limit of six, but for less time than the cap:
        the gate follows the job on, cancels nothing, and passes on its result."""
        self.env["NRESE_FLEET_SILENT_S"] = "120"
        self.play("status", state("running"), *SILENCE, *QUICK * 4, state("running"), state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (0, "fleet:gate-1@office-pc cpu=znver3", []),
                         f"SPEC silence-waited: a short silence ended the wait or cancelled the job\n{err}")

    def test_a_silence_that_reaches_the_cap_or_the_gates_limit_ends_with_one_cancel(self):
        """A silence ends the wait once it has lasted NRESE_FLEET_SILENT_S seconds, or the gate's
        MAX_S has passed, whichever comes first: no verdict, and the job, named, cancelled once. It
        is counted in seconds, not calls: two slow silent calls outlast a cap of three seconds."""
        silent = "the fleet didn't answer for \\d+ s"
        for silent_s, max_s, status, said in (("1", "10800", [state("running"), ""], silent),
                                              ("600", "3", ["~4"], "past 3 s"),
                                              ("3", "10800", [state("running"), "~2", "~2", state("done")], silent)):
            with self.subTest(silent_s=silent_s, max_s=max_s, status=status):
                self.reset()
                self.env.update(NRESE_FLEET_SILENT_S=silent_s, NRESE_GATE_MAX_S=max_s)
                self.play("status", *status)
                self.play("results", self.results())
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels(), self.calls("results")), (75, "", ["gate-1"], 0),
                                 f"SPEC silence-cap: not one cancel at the cap\n{err}")
                self.assertRegex(err, f"gate-1: {said}", "SPEC silence-cap: the job isn't named with the end of its wait")

    def test_no_fleet_call_starts_after_the_cap_or_the_gates_limit(self):
        """A poll longer than what is left of the wait: the next status or results call would start
        after the silence's cap or after MAX_S, so the wait ends there instead, with one cancel,
        whatever that call would have answered. Neither call reaches the fleet. (Whole seconds: the cap
        of 3 s is well past the silent call itself and well short of the 4 s poll. MAX_S is 4 s: the
        status and the first results call fit in it on a slow host; the 8 s poll runs past it.)"""
        for silent_s, max_s, poll_s, status, results, calls in (
                ("3", "10800", "4", [state("running"), "", state("done")], [], (2, 0)),
                ("30", "4", "8", [state("done")], [""], (1, 1))):
            with self.subTest(silent_s=silent_s, max_s=max_s):
                self.reset()
                self.env.update(NRESE_FLEET_SILENT_S=silent_s, NRESE_GATE_MAX_S=max_s, NRESE_GATE_POLL_S=poll_s)
                self.play("status", *status)
                self.play("results", *results, self.results())
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels(), (self.calls("status"), self.calls("results"))),
                                 (75, "", ["gate-1"], calls),
                                 f"SPEC no-call-after-deadline: a call started after the deadline\n{err}")

    def test_an_answer_that_came_after_a_deadline_is_not_taken(self):
        """A call that started inside the deadlines but answered after one of them: its answer isn't
        taken, however good. A status `done` that took longer than what was left of a silence's cap, and
        results that came after MAX_S or after the cap of a silence while fetching them: no verdict, one
        cancel, and (for the status) no results asked for."""
        for name, silent_s, max_s, status, results in (
                ("status after the cap", "2", "10800", ["", f"+3 {state('done')}"], []),
                ("results after MAX_S", "30", "1", [state("done")], ["+2"]),
                ("results after the cap", "2", "10800", [state("done")], ["", "+3"])):
            with self.subTest(name):
                self.reset()
                self.env.update(NRESE_FLEET_SILENT_S=silent_s, NRESE_GATE_MAX_S=max_s)
                self.play("status", *status)
                record = self.results()
                self.play("results", *[f"{r} {record}" if r.startswith("+") else r for r in results] or [record])
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                                 f"SPEC late-answer: an answer after the deadline gave the verdict\n{err}")
                if not results:
                    self.assertEqual(self.calls("results"), 0, "SPEC late-answer: results asked for after a late `done`")

    def test_a_status_call_that_failed_is_silence_whatever_it_printed(self):
        """The fleet's CLI prints a valid state and exits with an error: that is a CLI error, silence,
        not the job's state. Silence to the cap: no verdict, one cancel, no results asked for."""
        self.env["NRESE_FLEET_SILENT_S"] = "1"
        self.play("status", f"!{state('done')}")
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels(), self.calls("results")), (75, "", ["gate-1"], 0),
                         f"SPEC failed-call-silence: a failed call's output was taken as the job's state\n{err}")

    def test_no_results_call_starts_after_the_gates_limit(self):
        """The job's `done` comes from a status call that started inside MAX_S and answered after it:
        the results aren't asked for after MAX_S, so no verdict, with one cancel."""
        self.env["NRESE_GATE_MAX_S"] = "1"
        self.play("status", f"+2 {state('done')}")
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels(), (self.fake / "results-gate-1.n").exists()),
                         (75, "", ["gate-1"], False),
                         f"SPEC no-call-after-deadline: results were asked for after MAX_S\n{err}")

    def test_a_wait_past_its_deadline_ends_without_another_poll(self):
        """A call that comes back after a deadline has passed ends the wait at once: a status
        `running` answered after MAX_S, or a silent results call that outlasts the silence's cap. No
        verdict, one cancel, and no further poll interval slept first. The poll is 30 s, so a wait that
        slept it once more takes over 30 s; the bound of 25 s leaves room for a slow host's start-up."""
        self.env.update(NRESE_GATE_POLL_S="30", NRESE_FLEET_SILENT_S="1")
        for name, max_s, status, results in (("status", "1", [f"+2 {state('running')}"], []),
                                             ("results", "10800", [state("done")], ["~2"])):
            with self.subTest(call=name):
                self.reset()
                self.env["NRESE_GATE_MAX_S"] = max_s
                self.play("status", *status)
                self.play("results", *results, self.results())
                began = time.monotonic()
                code, where, _, err = self.gate()
                took = time.monotonic() - began
                self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                                 f"SPEC cap-at-once: not no verdict with one cancel\n{err}")
                self.assertLess(took, 25, f"SPEC cap-at-once: it slept a poll after the deadline ({took:.1f} s)\n{err}")

    def test_no_poll_is_slept_after_a_slow_host_passed_the_deadline(self):
        """A timely `running` answer, but taking it in on a slow host (the host's name read from it,
        here an `awk` that takes 5 s) outlasts MAX_S: the wait ends there, with no verdict and one
        cancel, and doesn't sleep its 30 s poll first. The bound of 25 s leaves room for a slow host's
        start-up."""
        shim = self.fake / "bin" / "awk"
        shim.write_text("#!/bin/sh\n[ \"$1\" = '{ print $2 }' ] && sleep 5\nexec /usr/bin/awk \"$@\"\n", newline="\n")
        shim.chmod(0o755)
        self.addCleanup(shim.unlink)
        self.env.update(NRESE_GATE_POLL_S="30", NRESE_GATE_MAX_S="4")
        self.play("status", state("running"))
        self.play("results", self.results())
        began = time.monotonic()
        code, where, _, err = self.gate()
        took = time.monotonic() - began
        self.assertEqual((code, where, self.cancels(), self.calls("status")), (75, "", ["gate-1"], 1),
                         f"SPEC cap-at-once: not no verdict with one cancel after one status call\n{err}")
        self.assertLess(took, 25, f"SPEC cap-at-once: it slept a poll after MAX_S ({took:.1f} s)\n{err}")

    def test_a_job_that_ended_while_the_fleet_was_silent_is_judged_by_its_result(self):
        """The job fails on its host while the fleet doesn't answer; once it answers again, inside
        the cap, the gate reads the job's result, and the verdict is the result's."""
        self.play("status", state("running"), *SILENCE, *QUICK, state("failed"))
        self.play("results", self.results(**FAILING))
        code, _, _, err = self.gate()
        self.assertEqual((code, self.cancels()), (1, []),
                         f"SPEC silence-ended-job: the job's own result wasn't the verdict\n{err}")
        self.assertIn("the tree fails the gate", err)

    def test_results_that_dont_come_are_asked_for_again(self):
        """After the job ended, a results call with no answer is silence: asked again inside the cap.
        Results that come but hold no gate record are an answer: no verdict, asked once."""
        self.play("status", state("done"))
        self.play("results", "", "!error: the fleet's service is unreachable", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where), (0, "fleet:gate-1@office-pc cpu=znver3"),
                         f"SPEC results-again: silent results ended the wait\n{err}")
        self.reset()
        (self.fake / "results-gate-1").mkdir()
        self.play("status", state("done"))
        self.play("results", (self.fake / "results-gate-1").as_posix())
        code, _, _, err = self.gate()
        self.assertEqual((code, (self.fake / "results-gate-1.n").read_text().strip()), (75, "1"),
                         f"SPEC results-answer: results without a record weren't the end\n{err}")

    def test_a_pending_jobs_start_window_doesnt_run_during_a_silence(self):
        """A pending job, the fleet silent for longer than the start window: still followed when it
        answers again, and it passes once it runs."""
        self.env["NRESE_GATE_START_S"] = "3"
        self.play("status", state("pending"), "~5", state("pending"), state("running"), state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (0, "fleet:gate-1@office-pc cpu=znver3", []),
                         f"SPEC silence-start-window: the silence ran the start window out\n{err}")

    def test_a_start_the_fleet_expects_in_time_holds_the_window_open(self):
        """Spec (22 Nov, the gate's start window): a pending job the fleet expects to
        start at E, from which the tier's estimate still ends within the gate's limit, isn't cancelled
        when the fixed window ends; the window reaches to E + START_S. Here the window is 3 s, the fleet
        expects the start 20 s on and the job starts after 6 s: it passes. The expected start is said
        once."""
        self.env.update(NRESE_GATE_START_S="3", NRESE_GATE_EST_MIN="1")
        at = expected(20)
        basis = "first in its host's order: when a job there is estimated to end"
        self.play("status", *[f"+1 {pending(expected_start=at, basis=basis)}"] * 6, state("running"), state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (0, "fleet:gate-1@office-pc cpu=znver3", []),
                         f"SPEC start-window-expected: a start expected in time didn't hold the window open\n{err}")
        self.assertEqual(err.count(f"gate-1: expected start {at[11:16]} ({basis})"), 1, err)

    def test_a_start_time_it_cant_read_is_never_taken_as_in_time(self):
        """Spec (22 Nov, the gate's start window): a start time the waiter couldn't read (none, null,
        not in the fleet's local YYYY-MM-DDTHH:MM:SS form, or the answer of a status call that failed,
        however good) is never taken as fitting:
        the fixed window holds, and the job is cancelled when it ends, with no verdict. Here the window is
        3 s and the job would start after 6 s, well inside what each start time, read as a time, would
        hold open: 30 to 90 s on, with a 1 min estimate and a 200 s limit. Each is made as its case runs."""
        def zoned():  # 30 s on, as an instant with its zone: the same time on any host's clock
            return (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=30)).isoformat("T", "seconds")

        cases = (
            ("none", lambda: [f"+1 {pending()}"] * 6),
            ("null", lambda: [f"+1 {pending(expected_start=None, basis='not estimated')}"] * 6),
            ("malformed", lambda: [f"+1 {pending(expected_start='soon')}"] * 6),
            ("a number", lambda: [f"+1 {pending(expected_start=1795330592)}"] * 6),
            ("a zone offset", lambda: [f"+1 {pending(expected_start=zoned())}"] * 6),
            ("no seconds", lambda: [f"+1 {pending(expected_start=expected(90)[:16])}"] * 6),
            ("a lowercase t", lambda: [f"+1 {pending(expected_start=expected(30).replace('T', 't'))}"] * 6),
            ("a failed call's", lambda: [f"+1 {pending()}", f"!{pending(expected_start=expected(30))}",
                                         *[f"+1 {pending()}"] * 5]))
        for name, replies in cases:
            with self.subTest(name):
                self.reset()
                self.env.update(NRESE_GATE_START_S="3", NRESE_GATE_EST_MIN="1", NRESE_GATE_MAX_S="200")
                self.play("status", *replies(), state("running"), state("done"))
                self.play("results", self.results())
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                                 f"SPEC start-window-unread: an unread start time held the window open\n{err}")
                self.assertRegex(err, "gate-1: no host started it within 3 s")
                self.assertNotIn("expected start", err)

    def test_a_start_expected_too_late_for_the_limit_keeps_the_fixed_window(self):
        """Spec (22 Nov, the gate's start window): a start the fleet expects so late that the
        tier's estimate would run past the gate's limit doesn't hold the window open; the fixed window
        ends the wait, and its message names the expected start. Here: 5 s on, a 2 min estimate, a
        100 s limit."""
        self.env.update(NRESE_GATE_START_S="3", NRESE_GATE_EST_MIN="2", NRESE_GATE_MAX_S="100")
        at = expected(5)
        self.play("status", *[f"+1 {pending(expected_start=at, basis='its reservation')}"] * 6, state("running"),
                  state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                         f"SPEC start-window-too-late: a start too late for the limit held the window open\n{err}")
        self.assertIn(f"gate-1: no host started it within 3 s (the fleet expects it at {at[11:16]}, past this "
                      "gate's limit)", err)

    def test_an_expected_start_that_goes_unanswered_ends_the_window_there(self):
        """A start the fleet expected in time, then answers with no estimate: the window still reaches
        to that expected start + START_S, never further without a new one, so the wait ends there with
        no verdict, well inside the gate's limit. Here: expected 8 s on, a window of 5 s (so the wait
        ends about 13 s on), a limit of 60 s."""
        self.env.update(NRESE_GATE_START_S="5", NRESE_GATE_EST_MIN="0", NRESE_GATE_MAX_S="60")
        began = time.monotonic()
        self.play("status", f"+1 {pending(expected_start=expected(8), basis='its reservation')}",
                  f"+1 {pending(expected_start=None, basis='not estimated')}")
        self.play("results", self.results())
        code, where, _, err = self.gate()
        took = time.monotonic() - began
        self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                         f"SPEC start-window-last-expected: the wait didn't end with one cancel\n{err}")
        self.assertRegex(err, "gate-1: no host started it within 5 s")
        self.assertGreater(took, 10.5, f"SPEC start-window-last-expected: it ended before E + START_S\n{err}")
        self.assertLess(took, 20, f"SPEC start-window-last-expected: it waited on past E + START_S\n{err}")

    def test_an_expected_start_now_too_late_drops_what_an_earlier_one_held(self):
        """Spec (22 Nov, the gate's start window): a start expected in time, then one the fleet expects
        too late for the gate's limit: the earlier one no longer holds the window open, so only the fixed
        window holds, and the job, which would start after it, is cancelled with no verdict. Here: a
        3 s window, first 20 s on, then 60 s on with a 1 min estimate and a 100 s limit, the job starting
        after 6 s."""
        self.env.update(NRESE_GATE_START_S="3", NRESE_GATE_EST_MIN="1", NRESE_GATE_MAX_S="100")
        self.play("status", f"+1 {pending(expected_start=expected(20), basis='its reservation')}",
                  *[f"+1 {pending(expected_start=expected(60), basis='its reservation')}"] * 5, state("running"),
                  state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                         f"SPEC start-window-now-too-late: an earlier expected start still held the window\n{err}")
        self.assertIn("past this gate's limit", err)

    def test_a_silence_doesnt_move_what_an_expected_start_holds(self):
        """Spec (22 Nov, the gate's start window): a silence pauses the fixed window only; the end an
        expected start holds the window open to is a time, which no silence moves. Here: a 1 s window,
        expected 10 s on (held to about 11 s), then 10 s of silence (the fixed window to about 12 s),
        then no estimate, and the job would start after about 16 s: it is cancelled with no verdict."""
        self.env.update(NRESE_GATE_START_S="1", NRESE_GATE_EST_MIN="0", NRESE_GATE_MAX_S="60")
        self.play("status", f"+1 {pending(expected_start=expected(10), basis='its reservation')}", "~10",
                  *[f"+1 {pending(expected_start=None, basis='not estimated')}"] * 5, state("running"), state("done"))
        self.play("results", self.results())
        code, where, _, err = self.gate()
        self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                         f"SPEC start-window-silence-held: the silence moved what the expected start held\n{err}")


if __name__ == "__main__":
    unittest.main()
