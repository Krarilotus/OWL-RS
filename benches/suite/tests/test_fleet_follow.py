"""The gate's waiter (scripts/lib/fleet-job.sh, fleet_follow and fleet_results) through a fleet that
is silent for a while: silence is unknown, not an end. It is counted in seconds per spell, up to
NRESE_FLEET_SILENT_S and never past the gate's MAX_S; nothing is cancelled for it until the wait ends
there, with one cancel; a job that ended meanwhile is judged by its result once the fleet answers.

Run: python -m unittest discover -s benches/suite/tests -p test_fleet_follow.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import json
import unittest

import test_push_gate as push  # the scratch repository and the gate's job records (a module: its tests aren't rerun here)

# A stand-in for the fleet's CLI whose status and results play $FAKE/<command>-<job>.seq, one reply
# a call, the last again once they are used up: "~N" waits N s and answers nothing (a call that
# times out), "!TEXT" prints TEXT and fails (a CLI error on stdout), any other line is the answer.
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
        for silent_s, max_s, silence in (("1", "10800", [""]), ("600", "3", [""]),
                                         ("3", "10800", ["~2", "~2", state("done")])):
            with self.subTest(silent_s=silent_s, max_s=max_s, silence=silence):
                self.reset()
                self.env.update(NRESE_FLEET_SILENT_S=silent_s, NRESE_GATE_MAX_S=max_s)
                self.play("status", state("running"), *silence)
                self.play("results", self.results())
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                                 f"SPEC silence-cap: not one cancel at the cap\n{err}")
                self.assertIn("gate-1: the fleet didn't answer for", err, "SPEC silence-cap: the job isn't named")

    def test_no_fleet_call_starts_after_the_cap_or_the_gates_limit(self):
        """A poll longer than what is left of the wait: the next status or results call would start
        after the silence's cap or after MAX_S, so the wait ends there instead, with one cancel,
        whatever that call would have answered."""
        self.env["NRESE_GATE_POLL_S"] = "2"
        for silent_s, max_s, status, results in (("1", "10800", [state("running"), "", state("done")], []),
                                                 ("30", "1", [state("done")], [""])):
            with self.subTest(silent_s=silent_s, max_s=max_s):
                self.reset()
                self.env.update(NRESE_FLEET_SILENT_S=silent_s, NRESE_GATE_MAX_S=max_s)
                self.play("status", *status)
                self.play("results", *results, self.results())
                code, where, _, err = self.gate()
                self.assertEqual((code, where, self.cancels()), (75, "", ["gate-1"]),
                                 f"SPEC no-call-after-deadline: a call after the deadline gave the verdict\n{err}")

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


if __name__ == "__main__":
    unittest.main()
