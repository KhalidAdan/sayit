# Verse — the dream app

"Slack if it was made for people who cared about communication." Thread-first chat,
heavily inspired by Quill (the Twitter-acquired company), where AI models are
*collaborators in chat*: select a message, spawn a thread, invite people, an AI listens
the whole time, pulls transcripts, files/assigns Linear or Jira tickets, and an agent
attacks the bug — all from the chat interface. Status: someday-dream, but it has hooks.
Notes from 2026-07-22.

## The core insight (sharper than the pitch)

Not "AI in chat." It's that **in engineering teams, the conversation is the source of
truth, and everything else is a degraded copy of it.** The bug gets *understood* in the
thread; then a human manually re-encodes that understanding into a Jira ticket — instantly
a worse, staler version. Ticket-writing is transcription labor. In Verse, the thread *is*
the ticket; Jira/Linear become downstream renderings of it. That's an inversion, not a
feature — the same "unstructured human input → structured work" thesis as voice+LLM.

## Why thread-first matters for the AI part

Quill's threading discipline (real topics, real endings — not infinite scroll with 🧵
apologies) is exactly what gives an AI collaborator a **bounded context**. "Listen to
this channel" is noise; "this thread is about the checkout regression, started Tuesday,
these five people" is a context window with a purpose. The structure Quill wanted for
humans is the structure models need.

## What nobody has built: chat-native agency

Every piece exists in isolation (Linear agents, Copilot agents assigned to issues, Devin
in Slack) but they're bolted onto chat apps not built for it — the agent slides reports
under the door. In Verse the agent is a *participant*: work-in-progress visible in the
thread, redirectable conversationally mid-task, like a junior engineer. Incumbents can't
easily copy this; their data model is a message firehose, not a conversation graph.

## The hard part is not the AI

**Chat apps are where startups go to die** — brutal network effects; teams don't move.
Quill itself died this death. The strategic question isn't "can I build it" (yes — it's a
web app plus API calls) but *what's the wedge*. Candidate: don't start as a Slack
replacement; start as **the place where bugs get discussed** — the incident-to-resolution
loop — spawned from a link pasted anywhere, so much better at that one loop that it earns
the right to grow.

## Surviving the "please fix claude" objection

Objection: in an agent-heavy world, people won't converse about issues — they'll type
"please fix" at an agent. Taken seriously, it comes out the other side as an argument
*for* Verse:

- Agents delete the **diagnosis** conversations. They amplify everything above:
  is this a bug or a spec change? which of the agent's three fixes, given one breaks
  mobile? ship the workaround or the real fix? "Please fix" works when *fixed* is
  unambiguous; interesting problems are interesting because fixed is a judgment call.
- Historical rhyme: compilers killed "fit it in 4KB" talk and moved discussion up to
  architecture; Stack Overflow killed repeated questions. Automation deletes the bottom
  layer of conversation and makes the top layer more consequential.
- The reframe that stuck: **Verse is mission control for a team that employs agents.**
  When one engineer dispatches five agents before lunch, the scarce thing is shared
  situational awareness — what's in flight, who approved what, which agent is stuck,
  what tradeoff got decided yesterday. Slack is a terrible cockpit; a thread-first graph
  where every unit of work has a conversation attached is a cockpit.
- Structural incumbents note: **Slack monetizes per human seat.** A world where agents do
  the work threatens their pricing model — a wallet problem, not a speed problem. The
  wedge belongs to whoever charges for *outcomes* (resolved incidents), not seats.

## The incident loop map (via Polylane)

Value chain: **detect → triage → decide → resolve → learn.**

- *Detect*: observability (Datadog, Honeycomb, Sentry, Axiom).
- *Triage*: PagerDuty; incident.io / Rootly / FireHydrant own "incident conversation in
  Slack" — the closest existing thing to the Verse wedge.
- *Detect→resolve, collapsed*: **[Polylane](https://polylane.com/)** — "nobody should be
  on-call in 2026." Proactive agents that watch production, investigate with your real
  tools, and fix autonomously. Founded by Boris Tane (Baselime → acquired by Cloudflare;
  led Cloudflare Workers observability). Sits *on top of* observability tools. Pre-launch,
  waitlist (as of July 2026). See their post
  ["I'm Betting My Company on Proactive Agents"](https://polylane.com/blog/proactive-agents/).

Full automation only works **below the judgment line** (disk-full, bad-deploy rollbacks).
Above it, every autonomous-remediation product needs an escalation path — and today that
escalates into a Slack channel where the agent's structured investigation flattens into
bot spam. **Polylane needs a Verse**: escalation should land as a thread pre-populated
with the investigation, evidence, proposed fixes and tradeoffs, the right humans invited,
and the agent still present and steerable. Decisions flow back down (tickets, dispatch)
and into the *learn* step.

Unclaimed territory: the **judgment layer** — the cockpit and the **decision-memory**.
Observability remembers metrics, Jira remembers tickets; nobody remembers *judgment*
(what the team decided last time and why — which next quarter's agents need as context).
Every dollar invested in automating the bottom of the loop makes the top more valuable.

## Next moves (someday)

- Keep mapping who's attacking which segment of the loop; the unclaimed spot only becomes
  visible on a full map.
- Let the design mutate by *living* in agent-heavy workflows for another year.
- The sayit dictation app exercises the same muscle: voice → intent → structured action.
