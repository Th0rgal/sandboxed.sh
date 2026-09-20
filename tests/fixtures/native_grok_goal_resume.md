This sanitized Grok 1.0.34 streaming-json capture was supplied by the coordinator
from a successful native `/goal` and `--resume SESSION -p '/goal resume'` canary
on Spark. It contains 420 events: 275 text, 35 tool calls, 76 tool updates,
33 available-command announcements, and one successful `end_turn` event.
No model calls are needed to replay it.

The `.txt` file is the expected accumulated assistant text. The final text event
(zero-based event 418) repeats exactly the deltas at events 345 through 416;
it must not be appended twice. Earlier goal phases and tool events remain in
the capture to exercise segment boundaries. An `available_commands` event
between the final deltas and snapshot must not reset that segment.

Parser tests replay lines and bounded node log reads, including partial JSON,
UTF-8 boundaries, and a final event without a trailing newline. Session identity,
tool counts, live text progress, and successful termination are asserted.

`native_grok_fixed_session.jsonl` is the coordinator's Grok 1.0.34 ordinary
hostname canary on Spark, invoked with `--session-id
a1778740-4e51-4de4-b8c3-0f16d68b07de`. Only its final `end` event carries
the session ID, and it matches the supplied UUID. This capture verifies the
preallocated identity contract without making new model calls.
