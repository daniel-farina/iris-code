// Interactive TUI inspired by claude-code's Ink-based REPL but trimmed
// down to essentials: scrollable transcript on top, status line in the
// middle, multi-line input at the bottom. Slash commands match hip's
// REPL conventions.

import { Box, Static, Text, render, useApp, useInput, useStdout } from 'ink';
import Spinner from 'ink-spinner';
import TextInput from 'ink-text-input';
import React, { type FC, useEffect, useRef, useState } from 'react';
// React is referenced by jsxFactory (tsconfig: jsx=react). Keep this
// import even though TypeScript can't see the usage in JSX.
void React;
import { runLoop } from '../agent.js';
import { setDelegateContext } from '../tools/delegate.js';
import { VERSION } from '../version.js';
import { type BgTaskInfo, bgTasks, shortLabel } from '../background_tasks.js';
import { MtplxClient } from '../client.js';
import { generateAutoSessionId } from '../config.js';
import { type ChatMessage, systemMessage, userMessage } from '../schema.js';
import { updateSession } from '../session_store.js';
import type { SidecarConfig } from '../sidecar.js';
import {
  formatTaskList,
  statusGlyph as taskStatusGlyph,
  taskManager,
  type Task,
} from '../task_manager.js';
import { DEFAULT_SYSTEM_PROMPT, withEnv } from '../system_prompt.js';
import { detectResetIntent } from '../topic_detect.js';
import {
  AUTO_COMPACT_TOKEN_THRESHOLD,
  estimateTokens,
  formatTokens,
  shouldAutoCompact,
} from './auto_compact.js';
import { nextHippoWord, randomHippoWord } from './hippo_words.js';
import { listLoops, parseLoopInput, scheduleLoop, stopAllLoops, stopLoop } from './loop.js';
import { emptyStats, formatPerTool, formatStats } from './stats.js';
import {
  HIPPO_DEEP,
  HIPPO_MOSS,
  HIPPO_MUSTARD,
  HIPPO_PURPLE,
  HIPPO_SHADOW,
  HIPPO_SOFT_RED,
  loadLogo,
} from './theme.js';
import { compactToolLabel } from './tool_label.js';

interface TuiFlags {
  url: string;
  model: string;
  session?: string;
  maxTokens: number;
  maxRounds: number;
  temperature: number;
  topP: number;
  topK: number;
  system?: string;
  postcommitDelayMs?: number;
  /** When false, suppresses the conv>9K-token auto-compact trigger. */
  autoCompact?: boolean;
  /** Sidecar model id (auto-detected from Ollama if undefined). */
  sidecarModel?: string;
  sidecarUrl?: string;
}

type TranscriptItem =
  | { kind: 'user'; text: string }
  | { kind: 'assistant'; text: string }
  | { kind: 'tool'; name: string; args: string; ok: boolean; body: string }
  | { kind: 'system'; text: string };

interface AppProps {
  flags: TuiFlags;
  initialSessionId: string;
  /** Conv array to seed the session with. Defaults to just the system
   *  prompt for a fresh start. Pass the persisted record's `conv` to
   *  resume. */
  startConv?: ChatMessage[];
  /** Sidecar running-summary lines from a previous run (preserved
   *  across restart so /compact can splice them in for free). */
  startRunningSummary?: string[];
  /** Task list snapshot to rehydrate the task manager with. */
  startTasks?: Task[];
  /** Next task id counter from the snapshot, so new tasks continue
   *  numbering instead of colliding with rehydrated ones. */
  startTaskNextId?: number;
}

const App: FC<AppProps> = ({
  flags,
  initialSessionId,
  startConv,
  startRunningSummary,
  startTasks,
  startTaskNextId,
}) => {
  const { exit } = useApp();
  const { stdout } = useStdout();
  // Build the initial transcript. For a fresh start: just the
  // session-id + help-hint banners. For a resumed session: same
  // banners + a "resumed N messages" line + a compact replay of the
  // prior conv so the user sees their history.
  const [transcript, setTranscript] = useState<TranscriptItem[]>(() => {
    const items: TranscriptItem[] = [
      { kind: 'system', text: `hip · ${flags.model} · session ${initialSessionId.slice(0, 30)}` },
      { kind: 'system', text: 'type a message · /help for commands · /quit to exit' },
    ];
    if (startConv && startConv.length > 1) {
      items.push({
        kind: 'system',
        text: `[resumed ${startConv.length} messages from this session]`,
      });
      // Replay user/assistant messages (skip system and tool_results -
      // tool calls are summarized in the prior assistant items).
      for (const m of startConv) {
        if (m.role === 'user' && typeof m.content === 'string') {
          items.push({ kind: 'user', text: m.content });
        } else if (m.role === 'assistant' && typeof m.content === 'string' && m.content.trim().length > 0) {
          items.push({ kind: 'assistant', text: m.content });
        }
      }
    }
    return items;
  });
  // In-progress streaming item, rendered OUTSIDE Ink's <Static> wrapper
  // so it can redraw without forcing the whole transcript to redraw
  // (which is what was causing the flicker). On round end, this gets
  // committed into `transcript` and cleared.
  const [liveText, setLiveText] = useState<string>('');
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState('');
  const [sessionId, setSessionId] = useState(initialSessionId);
  const [runtimeMaxTokens, setRuntimeMaxTokens] = useState(flags.maxTokens);
  const [runtimeMaxRounds, setRuntimeMaxRounds] = useState(flags.maxRounds);
  const [runtimeModel, setRuntimeModel] = useState(flags.model);
  const [runtimeTemperature, setRuntimeTemperature] = useState(flags.temperature);
  const [runtimeTopP, setRuntimeTopP] = useState(flags.topP);
  const [runtimeTopK, setRuntimeTopK] = useState(flags.topK);
  // Auto-/compact toggle: defaults to on (flags.autoCompact undefined or true).
  // Off when --no-auto-compact was passed or after /auto-compact off.
  const [autoCompactOn, setAutoCompactOn] = useState<boolean>(flags.autoCompact !== false);
  // /browser on|off: run headless-Chrome console check on demand.
  const [browserCheckUrl, setBrowserCheckUrl] = useState<string>('http://localhost:5173');
  // Browser-check enabled flag for the top status bar. /browser off
  // disables it; /browser on or /browser <url> re-enables.
  const [browserEnabled, setBrowserEnabled] = useState<boolean>(true);
  // Mirror of bgTasks for the top/bottom status bars. Updated via the
  // subscribe() callback so the TUI re-renders whenever a task starts,
  // stops, or emits output (preview text in the bar).
  const [bgTaskList, setBgTaskList] = useState<BgTaskInfo[]>([]);
  // Mirror of taskManager state for the task panel. Updated via the
  // subscribe() callback.
  const [tasks, setTasks] = useState<Task[]>([]);
  // When the model (or the user) advances to a new task, we set this
  // to the task that should drive the post-round reset. The reset
  // itself happens in dispatchPrompt's `finally` so the model's
  // current round can finish cleanly first.
  const pendingTaskResetRef = useRef<Task | null>(null);
  // Sidecar display state for the top bar. Tracked separately from
  // sidecarCfgRef because refs don't trigger re-renders.
  const [sidecarLabel, setSidecarLabel] = useState<string>('detecting…');
  // Seed conv from a resumed session if one was passed; otherwise
  // start with just the (env-prepended) system prompt. The startConv
  // already contains its own system message from the persisted record,
  // so we don't re-prepend one in that case.
  const convRef = useRef<ChatMessage[]>(
    startConv && startConv.length > 0
      ? [...startConv]
      : [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))],
  );
  // Sidecar running-summary: one line per round, used to short-circuit
  // expensive main-model summarize calls in /compact.
  const runningSummaryRef = useRef<string[]>(startRunningSummary ? [...startRunningSummary] : []);
  const sidecarCfgRef = useRef<SidecarConfig | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const [stats, setStats] = useState(() => emptyStats());
  // Per-tool start times so onToolResult can record duration into stats.perTool.
  const toolStartTimes = useRef<Record<string, number>>({});
  // Per-tool args captured at onToolStart, used at onToolResult time so
  // we can push the tool transcript item once with final body (Static
  // doesn't update items in place).
  const pendingToolArgsRef = useRef<Record<string, string>>({});
  // Prompt history. Push on submit; Ctrl-P walks back, Ctrl-O walks
  // forward. -1 means "not navigating, show whatever the user has typed".
  const historyRef = useRef<string[]>([]);
  const [histIdx, setHistIdx] = useState<number>(-1);
  const draftRef = useRef<string>('');
  // Input queue: messages typed while the model is busy get parked here
  // and drained one at a time after each dispatch finishes. Up arrow
  // when input is empty AND queue is non-empty enters queue-edit mode:
  // the user can scroll through queued items, Enter resends, Del/x
  // removes, Esc exits queue mode.
  const [queue, setQueue] = useState<string[]>([]);
  const [queueIdx, setQueueIdx] = useState<number>(-1); // -1 = not editing queue
  // Animated hippo-themed status word that rotates every ~1.5s while
  // busy. Mirrors claude-code's "Smithing... (12s)" pattern. The pool
  // lives in ui/hippo_words.ts. We also track when busy started so we
  // can render the elapsed seconds next to the word.
  const [hippoWord, setHippoWord] = useState<string>(randomHippoWord());
  const [busyElapsed, setBusyElapsed] = useState<number>(0);
  const busyStartRef = useRef<number>(0);
  // Claude-code-style Ctrl-C: first press cancels in-flight work; if
  // nothing is running, first press "arms" the exit (shows a hint) and
  // a second press within the timeout window confirms exit.
  const exitArmedRef = useRef<NodeJS.Timeout | null>(null);
  const disarmExit = () => {
    if (exitArmedRef.current) {
      clearTimeout(exitArmedRef.current);
      exitArmedRef.current = null;
    }
  };
  // Bootstrap: resolve sidecar once at mount. Explicit flag wins;
  // otherwise probe Ollama and pick a small model if installed.
  useEffect(() => {
    void (async () => {
      const { probeSidecar } = await import('../sidecar.js');
      const quietSidecar = process.env['HIP_QUIET_SIDECAR'] === '1';
      if (flags.sidecarModel) {
        sidecarCfgRef.current = {
          url: flags.sidecarUrl ?? 'http://localhost:11434',
          model: flags.sidecarModel,
        };
        setSidecarLabel(flags.sidecarModel);
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[sidecar enabled: ${flags.sidecarModel} @ ${flags.sidecarUrl ?? 'http://localhost:11434'}]`,
          },
        ]);
        return;
      }
      const status = await probeSidecar(flags.sidecarUrl);
      if (status.kind !== 'ok') setSidecarLabel('off');
      if (status.kind === 'ok') {
        sidecarCfgRef.current = status.config;
        setSidecarLabel(status.config.model);
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[sidecar auto-detected: ${status.config.model} @ ${status.config.url}]`,
          },
        ]);
      } else if (!quietSidecar && status.kind === 'no-ollama') {
        setSidecarLabel('off');
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: '[no sidecar - Ollama not reachable. /compact will use the main model (slower).\n  Install:  brew install ollama  (or curl from ollama.com), then  ollama pull gemma4:e2b\n  Suppress this hint:  HIP_QUIET_SIDECAR=1]',
          },
        ]);
      } else if (!quietSidecar) {
        setSidecarLabel('off');
        // Ollama up, but no preferred model installed.
        const top = status.kind === 'no-model' ? status.preferred[0] : 'gemma4:e2b';
        const list =
          status.kind === 'no-model' ? status.preferred.slice(0, 3).join(', ') : 'gemma4:e2b';
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[no sidecar - Ollama is running but no preferred small model is installed. /compact will use the main model.\n  Fix:  ollama pull ${top}  (or any of: ${list})\n  Suppress this hint:  HIP_QUIET_SIDECAR=1]`,
          },
        ]);
      }
    })();
    // Sidecar config doesn't change after mount, but include them to
    // satisfy the linter without a suppression comment.
  }, [flags.sidecarModel, flags.sidecarUrl]);

  // Mirror bgTasks state into React. Subscribe once at mount and clean
  // up on unmount. Each emit (start, stop, output chunk) re-pulls the
  // full list - cheap because the list is bounded to ~20 retained tasks.
  useEffect(() => {
    setBgTaskList(bgTasks.list());
    const unsub = bgTasks.subscribe(() => setBgTaskList(bgTasks.list()));
    return () => {
      unsub();
    };
  }, []);

  // Mirror taskManager state into React and react to lifecycle events.
  // - 'created'/'updated': refresh the local list so the panel updates.
  // - 'started': remember the active task so dispatchPrompt's finally
  //   block can run a between-task context reset after the round ends.
  //   We don't reset mid-round: the agent loop is in the middle of
  //   processing the task_next tool result; cutting its conv out from
  //   under it would orphan its in-flight messages.
  useEffect(() => {
    // Rehydrate the task manager from the resumed snapshot (if any)
    // BEFORE subscribing - the rehydrate emits 'cleared' which our
    // subscriber would otherwise see as a no-op.
    if (startTasks && startTasks.length > 0) {
      taskManager.rehydrate({ tasks: startTasks, nextNum: startTaskNextId });
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text: `[resumed ${startTasks.length} tasks - use /tasks to view]`,
        },
      ]);
    }
    setTasks(taskManager.list());
    const unsub = taskManager.subscribe((ev) => {
      setTasks(taskManager.list());
      if (ev.kind === 'started') {
        pendingTaskResetRef.current = ev.task;
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[task id=${ev.task.id} started: ${ev.task.subject} - context will reset after this round]`,
          },
        ]);
      }
      if (ev.kind === 'completed') {
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[task id=${ev.task.id} completed: ${ev.task.subject}]`,
          },
        ]);
      }
    });
    return () => {
      unsub();
    };
  }, []);
  // While busy, rotate the hippo word + tick the elapsed counter every
  // second. The hippoWord cycles independently every ~1.5s; elapsed
  // ticks at 1Hz. Both stop and reset on busy=false.
  useEffect(() => {
    if (!busy) {
      setBusyElapsed(0);
      return;
    }
    busyStartRef.current = performance.now();
    setHippoWord(randomHippoWord());
    const wordTimer = setInterval(() => {
      setHippoWord((w) => nextHippoWord(w));
    }, 1500);
    const elapsedTimer = setInterval(() => {
      setBusyElapsed(Math.floor((performance.now() - busyStartRef.current) / 1000));
    }, 1000);
    return () => {
      clearInterval(wordTimer);
      clearInterval(elapsedTimer);
    };
  }, [busy]);

  // Wraps Ink's exit() with a session-persist + resume hint printed to
  // stderr (survives Ink's screen restore). Mirrors claude-code's exit
  // banner that tells the user how to pick up where they left off.
  const exitWithResumeHint = () => {
    void persistCurrentSession();
    process.stderr.write(
      `\n[hip] to resume this session later, run:\n  hip --resume ${sessionId}\n`,
    );
    exit();
  };

  useInput((_input, key) => {
    if (key.ctrl && _input === 'c') {
      // 1) Cancel any in-flight stream.
      if (abortRef.current) {
        abortRef.current.abort();
        abortRef.current = null;
        setStatus('cancelled');
        disarmExit();
        return;
      }
      // 2) Otherwise arm-then-confirm exit (press twice within 3s).
      if (exitArmedRef.current) {
        disarmExit();
        exitWithResumeHint();
        return;
      }
      setStatus('press Ctrl-C again to exit');
      exitArmedRef.current = setTimeout(() => {
        exitArmedRef.current = null;
        setStatus('');
      }, 3000);
      return;
    }
    // Ctrl-D: immediate exit (Unix EOF convention).
    if (key.ctrl && _input === 'd') {
      exitWithResumeHint();
      return;
    }
    // Any other key disarms the pending exit.
    disarmExit();
    // Ctrl-L: clear transcript display (keep conversation).
    if (key.ctrl && _input === 'l') {
      setTranscript([
        { kind: 'system', text: '[transcript cleared - conversation history retained]' },
      ]);
    }
    // Esc: cancel any in-flight work AND clear the input. Single keystroke
    // covers both "stop what you're doing" and "abort typing".
    if (key.escape) {
      if (abortRef.current) {
        abortRef.current.abort();
        abortRef.current = null;
        setStatus('cancelled');
      }
      setInput('');
      setHistIdx(-1);
      setQueueIdx(-1); // exit queue-edit mode if active
      draftRef.current = '';
      return;
    }
    // Queue-edit mode: up arrow with empty input + non-empty queue puts
    // us into queue navigation. Subsequent up/down walks queued items;
    // Enter is handled by TextInput's onSubmit (resends the edited
    // text); Ctrl-X / Delete removes the current item; Esc exits.
    if (key.upArrow && input === '' && queue.length > 0 && queueIdx < 0) {
      setQueueIdx(queue.length - 1);
      setInput(queue[queue.length - 1] ?? '');
      return;
    }
    if (queueIdx >= 0) {
      // We're in queue-edit mode.
      if (key.upArrow) {
        const next = Math.max(0, queueIdx - 1);
        setQueueIdx(next);
        setInput(queue[next] ?? '');
        return;
      }
      if (key.downArrow) {
        const next = queueIdx + 1;
        if (next >= queue.length) {
          setQueueIdx(-1);
          setInput('');
        } else {
          setQueueIdx(next);
          setInput(queue[next] ?? '');
        }
        return;
      }
      // Ctrl-X removes the currently-selected queued item.
      if ((key.ctrl && _input === 'x') || key.delete) {
        setQueue((q) => q.filter((_, i) => i !== queueIdx));
        setQueueIdx(-1);
        setInput('');
        return;
      }
    }
    // History navigation. Up/Down arrows are claude-code parity; Ctrl-P
    // and Ctrl-O remain as readline-style aliases. The TextInput
    // component is single-line so arrows have no in-input meaning to
    // shadow.
    if (!busy && (key.upArrow || (key.ctrl && _input === 'p'))) {
      const h = historyRef.current;
      if (h.length === 0) return;
      const nextIdx = histIdx < 0 ? h.length - 1 : Math.max(0, histIdx - 1);
      if (histIdx < 0) draftRef.current = input;
      setHistIdx(nextIdx);
      setInput(h[nextIdx] ?? '');
      return;
    }
    if (!busy && (key.downArrow || (key.ctrl && _input === 'o'))) {
      const h = historyRef.current;
      if (histIdx < 0) return;
      const nextIdx = histIdx + 1;
      if (nextIdx >= h.length) {
        setHistIdx(-1);
        setInput(draftRef.current);
      } else {
        setHistIdx(nextIdx);
        setInput(h[nextIdx] ?? '');
      }
      return;
    }
    // Ctrl-N: new session (fresh conversation + new session id).
    if (key.ctrl && _input === 'n') {
      void persistCurrentSession();
      const newId = generateAutoSessionId();
      convRef.current = [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))];
      setSessionId(newId);
      setStats(emptyStats());
      setTranscript([
        { kind: 'system', text: `[new session: ${newId}]` },
        { kind: 'system', text: 'type a message · /help for commands · /quit to exit' },
      ]);
    }
  });

  async function persistCurrentSession(): Promise<void> {
    // Mirror runPrintMode: rewrite this session's record so /resume picks
    // it up on a future launch. Idempotent - safe to call after every turn.
    // Persists conv + sidecar running_summary + the task plan snapshot
    // so a restart restores the full session state, not just the conv.
    const conv = convRef.current;
    if (conv.length <= 1) return; // system prompt only, nothing to save
    const firstUser = conv.find((m) => m.role === 'user')?.content ?? '';
    const summary = runningSummaryRef.current.length > 0 ? [...runningSummaryRef.current] : undefined;
    const taskSnap = taskManager.snapshot();
    try {
      await updateSession({
        session_id: sessionId,
        ts_unix: Math.floor(Date.now() / 1000),
        cwd: process.cwd(),
        first_user: typeof firstUser === 'string' ? firstUser.slice(0, 200) : '',
        conv,
        running_summary: summary,
        tasks: taskSnap.tasks.length > 0 ? taskSnap.tasks : undefined,
        task_next_id: taskSnap.nextNum,
      });
    } catch {
      // Best-effort - don't crash the REPL if disk is full / locked.
    }
  }

  /** Runs the compact flow: asks the model for a terse summary of the
   *  current conv, then replaces the conv with [system, user(summary),
   *  assistant(ack)]. Invoked manually from /compact, or automatically
   *  when shouldAutoCompact fires after a round. The `trigger` arg
   *  changes the transcript line so the user can tell which path ran. */
  async function runCompact(trigger: 'manual' | 'auto'): Promise<void> {
    if (convRef.current.length <= 3) {
      if (trigger === 'manual') {
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: '[nothing to compact - conv is already short]' },
        ]);
      }
      return;
    }
    const originalCount = convRef.current.length;
    const tokensBefore = estimateTokens(convRef.current);
    setBusy(true);
    setStatus(trigger === 'auto' ? 'auto-compacting...' : 'compacting...');
    try {
      // FAST PATH: if the sidecar has been producing per-round summaries,
      // use them directly. Saves the 5-10s main-model summarize.
      const sideLines = runningSummaryRef.current.filter((s) => s.trim().length > 0);
      let summary: string;
      if (sideLines.length > 0) {
        summary = sideLines.map((s, i) => `${i + 1}. ${s}`).join('\n');
      } else {
        const client = new MtplxClient({
          baseUrl: flags.url,
          model: runtimeModel,
          sessionId,
        });
        const summarizeConv: ChatMessage[] = [
          ...convRef.current,
          {
            role: 'user',
            content:
              'Summarize the conversation above in 5-10 bullet points: what was asked, what was done (files changed, decisions made), and any unresolved threads. Be terse - this is for resuming the session, not for the user.',
          },
        ];
        const res = await client.stream(summarizeConv, undefined, {
          temperature: runtimeTemperature,
          top_p: runtimeTopP,
          top_k: runtimeTopK,
          max_tokens: 1500,
        });
        summary = res.content.trim();
      }
      if (!summary) {
        setTranscript((p) => [...p, { kind: 'system', text: '[compact failed - empty summary]' }]);
        return;
      }
      convRef.current = [
        systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT)),
        userMessage(`[Compacted session summary]\n${summary}`),
        { role: 'assistant', content: 'OK, continuing from the summary.' },
      ];
      const tokensAfter = estimateTokens(convRef.current);
      if (trigger === 'auto') {
        // Single-line banner the user requested - prints the before/after
        // token estimate so they know auto-compact fired (and why).
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[auto-compact at ${formatTokens(tokensBefore)} tokens → ${formatTokens(tokensAfter)}]`,
          },
        ]);
      } else {
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[compacted ${originalCount} msgs into a summary; conv is now ${convRef.current.length} msgs]\n\n${summary}`,
          },
        ]);
      }
    } catch (e) {
      setTranscript((p) => [
        ...p,
        { kind: 'system', text: `[compact error] ${(e as Error).message}` },
      ]);
    } finally {
      setBusy(false);
      setStatus('');
    }
  }

  async function dispatchPrompt(text: string) {
    const t = text.trim();
    if (!t) return;
    // If the model is busy, park the message in the queue instead of
    // dispatching now. Drained automatically when busy clears.
    if (busy) {
      setQueue((q) => [...q, t]);
      return;
    }
    // Record non-slash inputs in history (slash commands are mostly
    // one-off control; storing them clutters Ctrl-P navigation). Cap at
    // 100 to keep memory bounded.
    const h = historyRef.current;
    if (!t.startsWith('/') && !t.startsWith(':') && h[h.length - 1] !== t) {
      h.push(t);
      if (h.length > 100) h.shift();
    }
    setHistIdx(-1);
    draftRef.current = '';
    // Slash commands.
    if (t.startsWith('/') || t.startsWith(':')) {
      const handled = handleSlash(t);
      if (handled) return;
    }
    // Conservative reset-intent detection: if the user typed an
    // explicit "start over"/"reset"/"forget that"/"new task:" phrase
    // and the conv has real history, wipe it and reseed with just the
    // system prompt before sending. Same logic as the print-mode
    // shortcut so behavior is consistent across UIs.
    if (detectResetIntent(t, convRef.current.length)) {
      const sysMsg = convRef.current.find((m) => m.role === 'system');
      const before = convRef.current.length;
      convRef.current = sysMsg ? [sysMsg] : [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))];
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text: `[reset signal detected; cleared ${before - convRef.current.length} prior messages]`,
        },
      ]);
    }
    // Surface any bg-task completions that landed since the previous
    // turn. Show them in the transcript AND prepend a one-line system
    // note to the user message so the model knows to react (e.g. after
    // a build finishes, switch to running tests).
    const bgDone = bgTasks.drainCompletions();
    if (bgDone.length > 0) {
      const lines = bgDone
        .map(
          (d) =>
            `[bg ${d.id}] ${d.status}${
              d.exitCode !== undefined ? ` (exit ${d.exitCode})` : ''
            } - ${d.command}`,
        )
        .join('\n');
      setTranscript((prev) => [...prev, { kind: 'system', text: lines }]);
      // Prepend to the user turn so the round sees it. Marked with
      // <bg-notification> so the system_prompt rule about flagging
      // hardcoded/incomplete state still applies cleanly.
      const annotated = `<bg-notification>\n${lines}\n</bg-notification>\n${t}`;
      setTranscript((prev) => [...prev, { kind: 'user', text: t }]);
      convRef.current.push(userMessage(annotated));
    } else {
      setTranscript((prev) => [...prev, { kind: 'user', text: t }]);
      convRef.current.push(userMessage(t));
    }
    setBusy(true);
    setStatus(''); // let the hippo-word rotator render the default state

    // Streaming buffer + live region. Updates to liveText re-render
    // ONLY the live box, not the whole transcript (which is inside
    // <Static>). Throttle to ~12Hz so React still batches.
    let pendingAssistant = '';
    let lastFlush = 0;
    let flushTimer: NodeJS.Timeout | null = null;
    const STREAM_FLUSH_MS = 100;

    const flushLive = () => {
      flushTimer = null;
      lastFlush = performance.now();
      setLiveText(pendingAssistant);
    };

    const client = new MtplxClient({
      baseUrl: flags.url,
      model: runtimeModel,
      sessionId,
      onContent: (chunk) => {
        pendingAssistant += chunk;
        const now = performance.now();
        const elapsed = now - lastFlush;
        if (elapsed >= STREAM_FLUSH_MS) {
          flushLive();
        } else if (!flushTimer) {
          flushTimer = setTimeout(flushLive, STREAM_FLUSH_MS - elapsed);
        }
      },
      onTtft: () => {
        // No-op: rotator carries the visual indication of progress.
      },
    });

    abortRef.current = new AbortController();
    // Refresh the delegate-tool context so sub-agents inherit the
    // current runtime model / sampling / session. Set per-dispatch
    // rather than once at mount because /model / /temperature can
    // change these between turns.
    setDelegateContext({
      baseUrl: flags.url,
      model: runtimeModel,
      parentSessionId: sessionId,
      sampling: {
        temperature: runtimeTemperature,
        top_p: runtimeTopP,
        top_k: runtimeTopK,
        max_tokens: runtimeMaxTokens,
      },
    });
    try {
      await runLoop({
        client,
        conv: convRef.current,
        signal: abortRef.current.signal,
        sampling: {
          temperature: runtimeTemperature,
          top_p: runtimeTopP,
          top_k: runtimeTopK,
          max_tokens: runtimeMaxTokens,
        },
        maxRounds: runtimeMaxRounds,
        postcommitDelayMs: flags.postcommitDelayMs,
        // Mid-round auto-compact. Without this, long single-prompt
        // dispatches (one user message → 60+ rounds of work) drift
        // past MTPLX's session-bank size cliff (~24K tok), every turn
        // after becomes a 45s cold prefill. We compact at the config
        // default with sidecar summaries to stay warm.
        // autoCompactOn=false disables entirely.
        autoCompactThresholdTokens: autoCompactOn ? AUTO_COMPACT_TOKEN_THRESHOLD : 0,
        systemPromptForCompact: flags.system,
        sidecar: sidecarCfgRef.current ?? undefined,
        runningSummary: sidecarCfgRef.current ? runningSummaryRef.current : undefined,
        events: {
          onSidecarSummary: (line, total) => {
            // Don't spam the transcript with every summary - just bump
            // the count silently. Surfaced in /stats and at /compact.
            // (Future: show in a footer line.)
            void line;
            void total;
          },
          onCompactStart: (tokensBefore) => {
            setStatus(`auto-compacting (${tokensBefore} tok)...`);
            setTranscript((p) => [
              ...p,
              {
                kind: 'system',
                text: `[auto-compact triggered at ${tokensBefore} tokens]`,
              },
            ]);
          },
          onCompactDone: (tokensBefore, tokensAfter, msgsBefore) => {
            setStatus('');
            setTranscript((p) => [
              ...p,
              {
                kind: 'system',
                text: `[auto-compact: ${tokensBefore} → ${tokensAfter} tok (${msgsBefore} msgs collapsed) - prefix cache should stay warm]`,
              },
            ]);
          },
          onBetweenRounds: async (conv) => {
            // Apply a queued task-transition reset mid-loop. The
            // taskManager subscription set pendingTaskResetRef when a
            // task flipped to in_progress; we run the actual conv
            // rewrite here (rather than in dispatchPrompt's finally)
            // so the next round starts with a fresh narrow context.
            const taskToResetTo = pendingTaskResetRef.current;
            if (!taskToResetTo) return;
            pendingTaskResetRef.current = null;
            const sysMsg =
              conv.find((m) => m.role === 'system') ??
              systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT));
            const sideLines = runningSummaryRef.current.filter((s) => s.trim().length > 0);
            const summaryBlock =
              sideLines.length > 0
                ? `[prior-task summary]\n${sideLines.map((s, i) => `${i + 1}. ${s}`).join('\n')}\n\n`
                : '';
            const taskList = formatTaskList(taskManager.list());
            const reseed = userMessage(
              `${summaryBlock}[current task] id=${taskToResetTo.id}: ${taskToResetTo.subject}\n${taskToResetTo.description}\n\n[task list so far]\n${taskList}\n\nWork this task. When done, call task_update with id="${taskToResetTo.id}" status="completed", then task_next. Stick to the plan.`,
            );
            const beforeCount = conv.length;
            // Mutate conv in place so the agent loop sees the new
            // contents on its next stream call. NEVER replace the
            // reference - the loop captured it by reference.
            conv.length = 0;
            conv.push(sysMsg);
            conv.push(reseed);
            setTranscript((p) => [
              ...p,
              {
                kind: 'system',
                text: `[task reset mid-loop: dropped ${beforeCount - 2} msgs · reseeded with summary digest + task id=${taskToResetTo.id}]`,
              },
            ]);
          },
          onToolResultPrune: (pruned, charsSaved) => {
            setTranscript((p) => [
              ...p,
              {
                kind: 'system',
                text: `[pruned ${pruned} old tool result${pruned === 1 ? '' : 's'} (~${(charsSaved / 4).toFixed(0)} tok freed)]`,
              },
            ]);
          },
          onPostcommitWait: (maxMs) => {
            setStatus(`waiting up to ${(maxMs / 1000).toFixed(0)}s for postcommit`);
          },
          onPostcommitDone: (landed, elapsedMs) => {
            setStatus(
              landed
                ? `postcommit landed (+${(elapsedMs / 1000).toFixed(1)}s)`
                : `postcommit timed out (${(elapsedMs / 1000).toFixed(1)}s)`,
            );
          },
          onRound: (n, info) => {
            setStats((prev) => ({
              ...prev,
              rounds: prev.rounds + 1,
              completionTokens: prev.completionTokens + (info.ctok ?? 0),
            }));
            // Persist after every round, not just every dispatch.
            // Long single-prompt dispatches can run 20+ rounds; without
            // per-round persist, external monitors (and recovery on
            // crash) see nothing until the whole dispatch completes.
            // Cost: one async write per round (~1-2ms, off the hot path).
            void persistCurrentSession();
          },
          onToolStart: (name, args) => {
            toolStartTimes.current[name] = performance.now();
            // Store args until result fires; we push the tool item only
            // once with final body, since <Static> can't update in place.
            pendingToolArgsRef.current[name] = JSON.stringify(args).slice(0, 200);
            setStats((p) => ({ ...p, toolCalls: p.toolCalls + 1 }));
            // Commit the in-progress assistant content to transcript so
            // it stays above the upcoming tool item, then reset.
            if (flushTimer) {
              clearTimeout(flushTimer);
              flushTimer = null;
            }
            const finalText = pendingAssistant;
            if (finalText.trim().length > 0) {
              setTranscript((p) => [...p, { kind: 'assistant', text: finalText }]);
            }
            setLiveText('');
            pendingAssistant = '';
            lastFlush = 0;
            setStatus(name);
          },
          onToolResult: (name, ok, body) => {
            const startedAt = toolStartTimes.current[name];
            const ms = startedAt ? performance.now() - startedAt : 0;
            const args = pendingToolArgsRef.current[name] ?? '{}';
            setStats((p) => {
              const prev = p.perTool[name] ?? { calls: 0, errors: 0, totalMs: 0 };
              return {
                ...p,
                perTool: {
                  ...p.perTool,
                  [name]: {
                    calls: prev.calls + 1,
                    errors: prev.errors + (ok ? 0 : 1),
                    totalMs: prev.totalMs + ms,
                  },
                },
              };
            });
            setTranscript((p) => [...p, { kind: 'tool', name, args, ok, body }]);
            // Tool finished; let the rotator resume during the next think.
            setStatus('');
          },
        },
      });
      // Commit the final tail content to transcript and clear live.
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      const finalText = pendingAssistant;
      if (finalText.trim().length > 0) {
        setTranscript((p) => [...p, { kind: 'assistant', text: finalText }]);
      }
      setLiveText('');
    } catch (e) {
      setTranscript((p) => [...p, { kind: 'system', text: `[error] ${(e as Error).message}` }]);
    } finally {
      abortRef.current = null;
      setBusy(false);
      setStatus('');
      // Persist after every turn so the REPL session survives an exit.
      void persistCurrentSession();
      // Task-transition resets and auto-compact now both happen
      // MID-LOOP in the runLoop onBetweenRounds hook (above), not
      // here. This block used to fire at end-of-dispatch but for
      // long single-prompt dispatches (60+ rounds in one user
      // message) that meant resets/compacts never ran in time.
      // We still leave the end-of-dispatch auto-compact fallback as
      // a safety net for cases where the threshold was raised
      // mid-flight or the conv crossed it between dispatches.
      if (shouldAutoCompact(convRef.current, autoCompactOn)) {
        setTimeout(() => void runCompact('auto'), 0);
      }
      // Drain one queued message if any. Fires-and-forgets the recursion
      // so we don't stack-overflow on a huge queue (await would block
      // the React finally chain).
      setQueue((q) => {
        if (q.length === 0) return q;
        const [next, ...rest] = q;
        if (next) setTimeout(() => void dispatchPrompt(next), 0);
        return rest;
      });
    }
  }

  function handleSlash(cmd: string): boolean {
    const lower = cmd.toLowerCase();
    if (lower === '/quit' || lower === '/exit' || lower === ':q' || lower === ':quit') {
      exitWithResumeHint();
      return true;
    }
    if (lower === '/clear' || lower === '/cls' || lower === ':clear') {
      setTranscript([
        { kind: 'system', text: '[transcript cleared - conversation history retained]' },
      ]);
      return true;
    }
    if (lower === '/cwd' || lower === '/pwd' || lower === ':cwd') {
      setTranscript((p) => [...p, { kind: 'system', text: `cwd: ${process.cwd()}` }]);
      return true;
    }
    if (lower === '/persist' || lower === '/save') {
      void persistCurrentSession().then(() =>
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `[persisted session ${sessionId} to disk]` },
        ]),
      );
      return true;
    }
    if (lower === '/help' || lower === ':help') {
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text:
            'shortcuts:\n  Enter            send (or enqueue if model is busy)\n  Esc              cancel stream + clear input + exit queue-edit mode\n  Ctrl-C           cancel stream; press twice within 3s to exit\n  Ctrl-D           exit immediately (prints resume hint)\n  Up / Down        prev / next history; in queue-edit mode: walk queue\n  Ctrl-X / Del     (queue-edit mode) remove selected queued message\n  Ctrl-N           new session (persists current)\n  Ctrl-L           clear transcript\n\nqueue:\n  Typing while the model is busy appends to a queue (shown above input).\n  Each queued message dispatches automatically after the current turn.\n  Press Up with empty input to enter queue-edit mode; edit + Enter\n  rewrites the selected item, Ctrl-X removes it, Esc exits queue mode.\n\nslash commands:\n  /help                       this message\n  /new                        new session (persists current)\n  /clear, /cls                clear transcript (keeps conv)\n  /cwd, /pwd                  show current working directory\n  /persist, /save             save current conv to disk now\n  /quit                       exit (auto-persists)\n  /max-tokens N               set per-turn output cap (current ' +
            runtimeMaxTokens +
            ')\n  /max-rounds N               set agent loop cap (current ' +
            runtimeMaxRounds +
            ')\n  /model [id|default]         get/set the model (current ' +
            runtimeModel +
            ')\n  /top-p [0-1|default]        get/set sampler top-p\n  /top-k [int|default]        get/set sampler top-k\n  /temperature [0-2|default]  get/set sampler temperature (current ' +
            runtimeTemperature +
            ')\n  /info                       show full runtime settings\n  /loop <Nu> <prompt>         schedule recurring prompt (e.g. /loop 5m run tests)\n  /loop <prompt> every <Nu>   same, trailing form\n  /loops                      list active loops\n  /loop-stop <id|all>         cancel a loop\n  /sessions [N]               list N (default 10) most recent persisted sessions\n  /resume <id|last>           switch this REPL to a persisted session\n  /stats                      show round/tool/token/ms counters\n  /usage                      show per-tool call counts + avg ms\n  /tools                      list available tools\n  /compact                    summarize conv and reset (frees prefix cache, keeps gist)\n  /browser [url]              headless-Chrome console-error check (default http://localhost:5173)\n  /auto-compact [on|off]      toggle auto-/compact at ~9K tokens (current\n  /setup                      check MTPLX config drift\n  /setup-apply                restart MTPLX with recommended env (after /setup)\n  /tasks                      list the model task plan (created via task_create)\n  /next                       claim the next pending task (resets context next round)\n  /tasks-clear                drop all tasks from the manager\n  /browser on|off|<url>       toggle / set browser_check (status shown in top bar)' +
            (autoCompactOn ? 'on' : 'off') +
            ')',
        },
      ]);
      return true;
    }
    if (lower === '/new' || lower === ':new') {
      // Persist outgoing session before resetting so it's recoverable via
      // /resume <id> from the next launch.
      void persistCurrentSession();
      const newId = generateAutoSessionId();
      convRef.current = [systemMessage(withEnv(flags.system ?? DEFAULT_SYSTEM_PROMPT))];
      setSessionId(newId);
      setStats(emptyStats());
      taskManager.clear();
      runningSummaryRef.current.length = 0;
      setTranscript([{ kind: 'system', text: `[new session: ${newId}]` }]);
      return true;
    }
    // /loop [interval] <prompt>: schedule a recurring prompt fire in this
    // session (same shape as Claude Code's /loop skill). Examples:
    //   /loop 5m run the smoke tests
    //   /loop check the deploy every 20m
    // /loops lists active jobs; /loop-stop <id|all> cancels them.
    if (cmd.startsWith('/loop ') || cmd === '/loop' || cmd.startsWith(':loop ')) {
      const body = cmd.replace(/^[/:]loop\s*/, '');
      const parsed = parseLoopInput(body);
      if (parsed === null) {
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: 'dynamic /loop (no interval) not yet supported. usage: /loop <Nu> <prompt>  (e.g. /loop 5m run the tests)',
          },
        ]);
        return true;
      }
      if ('error' in parsed) {
        setTranscript((p) => [...p, { kind: 'system', text: parsed.error }]);
        return true;
      }
      const job = scheduleLoop(parsed.intervalMs, parsed.cadence, parsed.prompt, async (prompt) =>
        dispatchPrompt(prompt),
      );
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text: `[loop ${job.id}] scheduled ${job.cadence}: ${truncateLines(job.prompt, 2, 80).replace(/\n/g, ' / ')}\n  · stays alive for this REPL session only\n  · /loops to list · /loop-stop ${job.id} (or all) to cancel`,
        },
      ]);
      // Fire it once immediately - matches Claude Code's "run the parsed
      // prompt now, then schedule the next firing" semantics.
      void dispatchPrompt(parsed.prompt);
      return true;
    }
    if (lower === '/loops' || lower === ':loops') {
      const jobs = listLoops();
      if (jobs.length === 0) {
        setTranscript((p) => [...p, { kind: 'system', text: '[no scheduled loops]' }]);
      } else {
        const list = jobs
          .map(
            (j) =>
              `  ${j.id} · ${j.cadence} · fires=${j.fires} · ${truncateLines(j.prompt, 1, 60).slice(0, 60)}`,
          )
          .join('\n');
        setTranscript((p) => [...p, { kind: 'system', text: `scheduled loops:\n${list}` }]);
      }
      return true;
    }
    if (lower === '/stats' || lower === ':stats') {
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text:
            `session ${sessionId}\nconv messages: ${convRef.current.length}\n` + formatStats(stats),
        },
      ]);
      return true;
    }
    if (lower === '/usage' || lower === ':usage') {
      setTranscript((p) => [...p, { kind: 'system', text: formatPerTool(stats) }]);
      return true;
    }
    // /compact: ask the model to summarize the current conv, then
    // replace it with [system, user(summary), assistant(ack)]. Keeps the
    // prefix cache small for long-running iterative work.
    if (lower === '/compact' || lower === ':compact') {
      void runCompact('manual');
      return true;
    }
    // /tasks: print the current plan (whatever the model has created
    // via task_create). Read-only - the model still owns the list.
    if (lower === '/tasks' || lower === ':tasks') {
      const list = taskManager.list();
      const c = taskManager.counts();
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text:
            list.length === 0
              ? '[no tasks - the model will create them as needed]'
              : `tasks: ${c.completed}/${c.total} done\n${formatTaskList(list)}`,
        },
      ]);
      return true;
    }
    // /tasks-clear: drop all tasks from the manager. Use when the user
    // pivots to a completely different request and wants a clean slate.
    if (lower === '/tasks-clear' || lower === ':tasks-clear') {
      taskManager.clear();
      setTranscript((p) => [...p, { kind: 'system', text: '[task list cleared]' }]);
      return true;
    }
    // /next: manually advance to the next pending task. Same effect as
    // the model calling task_next - flips status to in_progress and
    // arms a context reset for the next round.
    if (lower === '/next' || lower === ':next') {
      const n = taskManager.next();
      if (!n) {
        setTranscript((p) => [...p, { kind: 'system', text: '[no pending tasks]' }]);
      } else {
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text: `[advanced to id=${n.id}: ${n.subject} - context will reset on the next round]`,
          },
        ]);
      }
      return true;
    }
    // /setup: detect MTPLX config drift + offer to fix from inside the
    // TUI. Doesn't use readline (Ink owns stdin) - instead shows the
    // diff and a follow-up /setup-apply command the user can type.
    if (lower === '/setup' || lower === ':setup') {
      void (async () => {
        const { getMtplxStatus, diffConfig, formatDiff, RECOMMENDED_ENV } = await import(
          '../mtplx_manager.js'
        );
        const status = await getMtplxStatus(flags.url);
        const diff = diffConfig(status.env, RECOMMENDED_ENV);
        if (!status.running) {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: `[setup] MTPLX is not reachable at ${flags.url}` },
          ]);
          return;
        }
        if (diff.missing.length === 0) {
          setTranscript((p) => [
            ...p,
            {
              kind: 'system',
              text: `[setup] MTPLX is already configured for hippo-code (pid ${status.pid}, ${diff.matching.length} matching).`,
            },
          ]);
          return;
        }
        setTranscript((p) => [
          ...p,
          {
            kind: 'system',
            text:
              formatDiff(status, diff) +
              '\n\nType `/setup-apply` to SIGTERM the running MTPLX and relaunch ' +
              'it with the recommended env vars (~10s downtime).',
          },
        ]);
      })();
      return true;
    }
    if (lower === '/setup-apply' || lower === ':setup-apply') {
      void (async () => {
        setBusy(true);
        setStatus('restarting MTPLX...');
        try {
          const {
            getMtplxStatus,
            diffConfig,
            RECOMMENDED_ENV,
            stopMtplx,
            startMtplx,
            waitForMtplx,
          } = await import('../mtplx_manager.js');
          const status = await getMtplxStatus(flags.url);
          if (!status.running || !status.pid || !status.cmdline) {
            setTranscript((p) => [
              ...p,
              { kind: 'system', text: '[setup-apply] MTPLX not running; nothing to restart.' },
            ]);
            return;
          }
          const diff = diffConfig(status.env, RECOMMENDED_ENV);
          if (diff.missing.length === 0) {
            setTranscript((p) => [
              ...p,
              { kind: 'system', text: '[setup-apply] already configured; no restart needed.' },
            ]);
            return;
          }
          const port = (flags.url.match(/:(\d+)/) ?? [undefined, '8088'])[1] ?? '8088';
          const desiredEnv = { ...(status.env ?? {}), ...RECOMMENDED_ENV };
          const cmdline =
            status.venvPython && status.cmdline[0] !== status.venvPython
              ? [status.venvPython, ...status.cmdline.slice(1)]
              : status.cmdline;
          await stopMtplx(status.pid, port);
          const newPid = startMtplx(cmdline, desiredEnv);
          await waitForMtplx(flags.url, 90000);
          setTranscript((p) => [
            ...p,
            {
              kind: 'system',
              text: `[setup-apply] MTPLX restarted (new pid ${newPid}) with recommended env.`,
            },
          ]);
        } catch (e) {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: `[setup-apply error] ${(e as Error).message}` },
          ]);
        } finally {
          setBusy(false);
          setStatus('');
        }
      })();
      return true;
    }
    // /browser [url|on|off]: run a headless-Chrome console check now and
    // show any errors found. `/browser <url>` updates the default URL.
    // `/browser off` disables the browser feature (shown in top bar);
    // `/browser on` re-enables.
    const bcm = cmd.match(/^[/:]browser(?:\s+(\S+))?\s*$/i);
    if (bcm) {
      const arg = bcm[1];
      if (arg === 'off') {
        setBrowserEnabled(false);
        setTranscript((p) => [...p, { kind: 'system', text: 'browser_check disabled' }]);
        return true;
      }
      if (arg === 'on') {
        setBrowserEnabled(true);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `browser_check enabled (url: ${browserCheckUrl})` },
        ]);
        return true;
      }
      if (arg?.startsWith('http')) {
        setBrowserCheckUrl(arg);
        setBrowserEnabled(true);
      }
      const target = arg?.startsWith('http') ? arg : browserCheckUrl;
      setTranscript((p) => [
        ...p,
        { kind: 'system', text: `running browser_check on ${target}...` },
      ]);
      void (async () => {
        const { browserCheck, formatBrowserCheck } = await import('../browser_check.js');
        const r = await browserCheck({ url: target });
        setTranscript((p) => [...p, { kind: 'system', text: formatBrowserCheck(r) }]);
      })();
      return true;
    }
    // /auto-compact on|off: toggle the conv>9K-token auto-compact trigger.
    const acm = lower.match(/^[/:]auto-compact(?:\s+(on|off))?\s*$/);
    if (acm) {
      const arg = acm[1];
      if (!arg) {
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `auto-compact = ${autoCompactOn ? 'on' : 'off'}` },
        ]);
      } else {
        setAutoCompactOn(arg === 'on');
        setTranscript((p) => [...p, { kind: 'system', text: `auto-compact = ${arg}` }]);
      }
      return true;
    }
    if (lower === '/tools' || lower === ':tools') {
      void (async () => {
        const { REGISTRY } = await import('../tools/index.js');
        const rows = REGISTRY.map((t) => {
          const desc = t.spec.function.description ?? '';
          return `  ${t.name.padEnd(10)} ${desc.split('\n')[0]?.slice(0, 70) ?? ''}`;
        }).join('\n');
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `${REGISTRY.length} tools available:\n${rows}` },
        ]);
      })();
      return true;
    }
    if (lower === '/info' || lower === ':info') {
      setTranscript((p) => [
        ...p,
        {
          kind: 'system',
          text:
            `session    ${sessionId}\n` +
            `cwd        ${process.cwd()}\n` +
            `url        ${flags.url}\n` +
            `model      ${runtimeModel}\n` +
            `temp       ${runtimeTemperature}  top_p=${runtimeTopP}  top_k=${runtimeTopK}\n` +
            `max-tokens ${runtimeMaxTokens}\n` +
            `max-rounds ${runtimeMaxRounds}\n` +
            `conv msgs  ${convRef.current.length}`,
        },
      ]);
      return true;
    }
    const mm = cmd.match(/^[/:]model(?:\s+(\S+))?/i);
    if (mm) {
      const arg = mm[1];
      if (!arg) {
        setTranscript((p) => [...p, { kind: 'system', text: `model = ${runtimeModel}` }]);
      } else if (arg === 'default') {
        setRuntimeModel(flags.model);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `model reset to default (${flags.model})` },
        ]);
      } else {
        setRuntimeModel(arg);
        setTranscript((p) => [...p, { kind: 'system', text: `model = ${arg}` }]);
      }
      return true;
    }
    const tpm = lower.match(/^[/:]top-p(?:\s+(\S+))?/);
    if (tpm) {
      const arg = tpm[1];
      if (!arg) {
        setTranscript((p) => [...p, { kind: 'system', text: `top-p = ${runtimeTopP}` }]);
      } else if (arg === 'default') {
        setRuntimeTopP(flags.topP);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `top-p reset to default (${flags.topP})` },
        ]);
      } else {
        const n = Number(arg);
        if (Number.isFinite(n) && n > 0 && n <= 1) {
          setRuntimeTopP(n);
          setTranscript((p) => [...p, { kind: 'system', text: `top-p = ${n}` }]);
        } else {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: 'usage: /top-p <0.0-1.0> | default' },
          ]);
        }
      }
      return true;
    }
    const tkm = lower.match(/^[/:]top-k(?:\s+(\S+))?/);
    if (tkm) {
      const arg = tkm[1];
      if (!arg) {
        setTranscript((p) => [...p, { kind: 'system', text: `top-k = ${runtimeTopK}` }]);
      } else if (arg === 'default') {
        setRuntimeTopK(flags.topK);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `top-k reset to default (${flags.topK})` },
        ]);
      } else {
        const n = Number(arg);
        if (Number.isFinite(n) && Number.isInteger(n) && n > 0) {
          setRuntimeTopK(n);
          setTranscript((p) => [...p, { kind: 'system', text: `top-k = ${n}` }]);
        } else {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: 'usage: /top-k <positive integer> | default' },
          ]);
        }
      }
      return true;
    }
    const tm = lower.match(/^[/:]temperature(?:\s+(\S+))?/);
    if (tm) {
      const arg = tm[1];
      if (!arg) {
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `temperature = ${runtimeTemperature}` },
        ]);
      } else if (arg === 'default') {
        setRuntimeTemperature(flags.temperature);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `temperature reset to default (${flags.temperature})` },
        ]);
      } else {
        const n = Number(arg);
        if (Number.isFinite(n) && n >= 0 && n <= 2) {
          setRuntimeTemperature(n);
          setTranscript((p) => [...p, { kind: 'system', text: `temperature = ${n}` }]);
        } else {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: 'usage: /temperature <0.0-2.0> | default' },
          ]);
        }
      }
      return true;
    }
    // /sessions [N]: list N most recent persisted sessions (default 10).
    // /resume <id|last>: switch the current conv to a persisted session.
    const sessMatch = cmd.match(/^[/:]sessions(?:\s+(\d+))?\s*$/i);
    if (sessMatch) {
      const n = sessMatch[1] ? Math.max(1, Math.min(200, Number(sessMatch[1]))) : 10;
      void (async () => {
        const { readAllSessions } = await import('../session_store.js');
        const here = process.cwd();
        const all = (await readAllSessions()).slice(-n).reverse();
        if (all.length === 0) {
          setTranscript((p) => [...p, { kind: 'system', text: '[no persisted sessions yet]' }]);
        } else {
          const list = all
            .map((r) => {
              const when = new Date(r.ts_unix * 1000).toISOString().slice(0, 19).replace('T', ' ');
              const tag = r.cwd === here ? ' [here]' : '';
              return `  ${when}  ${r.session_id.slice(0, 30).padEnd(30)}  msgs=${String(r.conv.length).padStart(3)}${tag}  ${r.first_user.slice(0, 40)}`;
            })
            .join('\n');
          setTranscript((p) => [
            ...p,
            {
              kind: 'system',
              text: `recent sessions (newest first; [here] = same cwd):\n${list}\n  /resume <id> to switch · /resume last for the newest`,
            },
          ]);
        }
      })();
      return true;
    }
    const resumeMatch = cmd.match(/^[/:]resume(?:\s+(\S+))?/i);
    if (resumeMatch) {
      const id = resumeMatch[1];
      void (async () => {
        const { findSession, lastSession, lastSessionForCwd } = await import('../session_store.js');
        // `/resume last` (or bare `/resume`) prefers this-cwd's most
        // recent, parity with --continue-last. Falls back to global.
        const lastFor = (await lastSessionForCwd(process.cwd())) ?? (await lastSession());
        const rec = !id || id === 'last' ? lastFor : await findSession(id);
        if (!rec) {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: id ? `no session ${id}` : 'no prior sessions' },
          ]);
          return;
        }
        convRef.current = [...rec.conv];
        setSessionId(rec.session_id);
        setTranscript([
          { kind: 'system', text: `[resumed ${rec.session_id} (${rec.conv.length} msgs)]` },
          { kind: 'system', text: `first user: ${rec.first_user.slice(0, 100)}` },
        ]);
      })();
      return true;
    }
    const stopMatch = cmd.match(/^[/:]loop-stop\s+(\S+)/i);
    if (stopMatch) {
      const id = stopMatch[1]!;
      if (id === 'all') {
        const n = stopAllLoops();
        setTranscript((p) => [...p, { kind: 'system', text: `[stopped ${n} loop(s)]` }]);
      } else if (stopLoop(id)) {
        setTranscript((p) => [...p, { kind: 'system', text: `[loop ${id} cancelled]` }]);
      } else {
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `no loop with id ${id}; /loops to list active jobs` },
        ]);
      }
      return true;
    }
    const mt = lower.match(/^[/:]max-tokens(?:\s+(\S+))?/);
    if (mt) {
      const arg = mt[1];
      if (!arg) {
        setTranscript((p) => [...p, { kind: 'system', text: `max-tokens = ${runtimeMaxTokens}` }]);
      } else if (arg === 'default') {
        setRuntimeMaxTokens(flags.maxTokens);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `max-tokens reset to default (${flags.maxTokens})` },
        ]);
      } else {
        const n = Number(arg);
        if (Number.isFinite(n) && n > 0) {
          setRuntimeMaxTokens(n);
          setTranscript((p) => [...p, { kind: 'system', text: `max-tokens = ${n}` }]);
        } else {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: 'usage: /max-tokens <positive integer> | default' },
          ]);
        }
      }
      return true;
    }
    const mr = lower.match(/^[/:]max-rounds(?:\s+(\S+))?/);
    if (mr) {
      const arg = mr[1];
      if (!arg) {
        setTranscript((p) => [...p, { kind: 'system', text: `max-rounds = ${runtimeMaxRounds}` }]);
      } else if (arg === 'default') {
        setRuntimeMaxRounds(flags.maxRounds);
        setTranscript((p) => [
          ...p,
          { kind: 'system', text: `max-rounds reset to default (${flags.maxRounds})` },
        ]);
      } else {
        const n = Number(arg);
        if (Number.isFinite(n) && n > 0) {
          setRuntimeMaxRounds(n);
          setTranscript((p) => [...p, { kind: 'system', text: `max-rounds = ${n}` }]);
        } else {
          setTranscript((p) => [
            ...p,
            { kind: 'system', text: 'usage: /max-rounds <positive integer> | default' },
          ]);
        }
      }
      return true;
    }
    return false;
  }

  const width = stdout?.columns ?? 80;
  const runningBg = bgTaskList.filter((t) => t.status === 'running');
  const cwdLabel = compactPath(process.cwd(), Math.max(20, Math.floor(width * 0.4)));

  return (
    <Box flexDirection="column" width={width}>
      {/* Settled transcript items render once via <Static> and never
          redraw — this is what kills the flicker. Static appends to
          the terminal as items grow but doesn't touch prior items
          when the rest of the app re-renders. */}
      <Static items={transcript}>
        {(item, i) => <TranscriptLine key={i} item={item} width={width} />}
      </Static>
      {/* Top status bar of the live region: cwd · sidecar · browser
          on the left, hip version on the right. Sits just below
          scrollback and re-renders when any of those values change.
          NOT inside <Static> because <Static> would freeze its
          content at first render. */}
      <Box justifyContent="space-between" width={width}>
        <Text color={HIPPO_SHADOW}>
          {`▎ cwd: `}
          <Text color={HIPPO_PURPLE}>{cwdLabel}</Text>
          {`  ·  sidecar: `}
          <Text color={sidecarLabel === 'off' ? HIPPO_DEEP : HIPPO_MOSS}>{sidecarLabel}</Text>
          {`  ·  browser: `}
          <Text color={browserEnabled ? HIPPO_MOSS : HIPPO_DEEP}>
            {browserEnabled ? 'enabled' : 'disabled'}
          </Text>
        </Text>
        <Text color={HIPPO_DEEP}>{`hip ${VERSION} ▎`}</Text>
      </Box>
      {/* Live streaming text: only this re-renders during streaming.
          Empty string = no live item showing. */}
      {liveText.length > 0 && (
        <Box flexDirection="column">
          <Text>{wrapToWidth(liveText, Math.max(20, width - 4))}</Text>
        </Box>
      )}
      {/* Queue: messages typed while the model was busy. Shows just
          above the status line. Up arrow with empty input enters
          queue-edit mode. */}
      {queue.length > 0 && (
        <Box flexDirection="column" marginTop={1}>
          <Text color={HIPPO_SHADOW}>
            queued ({queue.length}) — Up to edit, Ctrl-X to remove, Esc to exit
          </Text>
          {queue.map((q, i) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: queue is positionally stable for visual indication
            <Text key={i} color={i === queueIdx ? HIPPO_MUSTARD : HIPPO_DEEP}>
              {`  ${i === queueIdx ? '►' : ' '} ${q.slice(0, 80)}${q.length > 80 ? '…' : ''}`}
            </Text>
          ))}
        </Box>
      )}
      <Box marginTop={1}>
        <Text color="gray">
          {`session ${sessionId.slice(0, 30)} · ${formatStats(stats)} · /help`}
          {convRef.current.length > 30 ? ' · /compact recommended' : ''}
        </Text>
      </Box>
      {/* Status row: spinner + animated hippo-themed verb + elapsed
          seconds while busy. The input stays mounted below so the
          user can type into the queue without losing the spinner.
          Specific transient messages (e.g. "press Ctrl-C again to
          exit") override the rotation when status is non-empty. */}
      {busy && (
        <Box>
          <Spinner type="dots" />
          <Text color={HIPPO_MUSTARD}>{` ${status || `${hippoWord}...`}`}</Text>
          <Text dimColor>{` · ${formatElapsed(busyElapsed)} since prompt`}</Text>
        </Box>
      )}
      {/* Task panel: shows the model's plan (created via task_create)
          and progress through it. Hidden when there are no tasks. */}
      {tasks.length > 0 && (
        <Box flexDirection="column" marginTop={1}>
          <Text color={HIPPO_SHADOW}>
            {(() => {
              const c = taskManager.counts();
              return `tasks: ${c.completed}/${c.total} done${
                c.inProgress > 0 ? ` · ${c.inProgress} active` : ''
              }${c.pending > 0 ? ` · ${c.pending} pending` : ''}  (task_list / task_next)`;
            })()}
          </Text>
          {tasks.slice(0, 6).map((t) => (
            <Text
              key={t.id}
              color={
                t.status === 'in_progress'
                  ? HIPPO_MUSTARD
                  : t.status === 'completed'
                    ? HIPPO_MOSS
                    : t.status === 'deleted'
                      ? HIPPO_SOFT_RED
                      : HIPPO_DEEP
              }
            >
              {`  ${taskStatusGlyph(t.status)} id=${t.id}  ${
                t.subject.length > Math.max(20, width - 14)
                  ? t.subject.slice(0, Math.max(20, width - 15)) + '…'
                  : t.subject
              }`}
            </Text>
          ))}
          {tasks.length > 6 && <Text dimColor>{`  … ${tasks.length - 6} more`}</Text>}
        </Box>
      )}
      {/* Bottom bg-task bar: list of active background tasks (running)
          plus a one-line preview of the most recent output. Only
          renders when at least one task is active OR has been retained
          after finishing in the current session. */}
      {bgTaskList.length > 0 && (
        <Box flexDirection="column" marginTop={1}>
          <Text color={HIPPO_MUSTARD}>
            {`bg tasks: ${runningBg.length} running`}
            {bgTaskList.length > runningBg.length
              ? ` · ${bgTaskList.length - runningBg.length} finished`
              : ''}
            {`  (bg_list / bg_output / bg_stop)`}
          </Text>
          {bgTaskList.slice(0, 4).map((t) => (
            <Text
              key={t.id}
              color={
                t.status === 'running'
                  ? HIPPO_MOSS
                  : t.status === 'completed'
                    ? HIPPO_DEEP
                    : HIPPO_SOFT_RED
              }
            >
              {`  ${statusGlyph(t.status)} ${t.id}  ${shortLabel(t, 28).padEnd(28)}  ${
                t.status === 'running'
                  ? (t.preview || '(no output yet)').slice(0, Math.max(20, width - 50))
                  : `${t.status}${t.exitCode !== undefined ? ` (exit ${t.exitCode})` : ''}`
              }`}
            </Text>
          ))}
          {bgTaskList.length > 4 && (
            <Text dimColor>{`  … ${bgTaskList.length - 4} more`}</Text>
          )}
        </Box>
      )}
      <Box>
        <Text color={busy ? HIPPO_DEEP : HIPPO_PURPLE}>{'> '}</Text>
        <TextInput
          value={input}
          onChange={setInput}
          onSubmit={(value) => {
            const v = value.trim();
            setInput('');
            // In queue-edit mode: replace the selected item (or remove
            // and dispatch immediately if not busy). Submit while busy
            // appends to the queue (handled inside dispatchPrompt).
            if (queueIdx >= 0) {
              if (!v) {
                // Empty submit while editing = delete.
                setQueue((q) => q.filter((_, i) => i !== queueIdx));
              } else if (busy) {
                setQueue((q) => q.map((item, i) => (i === queueIdx ? v : item)));
              } else {
                // Not busy + editing → remove from queue and send now.
                setQueue((q) => q.filter((_, i) => i !== queueIdx));
                void dispatchPrompt(v);
              }
              setQueueIdx(-1);
              return;
            }
            void dispatchPrompt(v);
          }}
          placeholder={
            busy
              ? 'queueing — Enter to add, Up to edit queue, Esc to clear input'
              : 'message (Enter to send · /help · Ctrl-C to exit)'
          }
        />
      </Box>
    </Box>
  );
};

const TranscriptLine: FC<{ item: TranscriptItem; width: number }> = ({ item, width }) => {
  const w = Math.max(20, width - 4);
  switch (item.kind) {
    case 'user':
      // Show user messages in full - never truncate. They're what the
      // user typed; truncating loses context the user explicitly
      // provided and surprises them. Terminal scrollback handles
      // long messages naturally.
      return (
        <Box flexDirection="column">
          <Text color={HIPPO_PURPLE}>{'▶ ' + wrapToWidth(item.text, w)}</Text>
        </Box>
      );
    case 'assistant':
      // Show assistant messages in full, including any <think>…</think>
      // blocks the reasoning model emits. We don't filter or cap -
      // letting the user see the model's reasoning is part of trust.
      return (
        <Box flexDirection="column">
          <Text>{wrapToWidth(item.text, w)}</Text>
        </Box>
      );
    case 'tool': {
      const head = `· ${compactToolLabel(item.name, item.args)}`;
      const body = item.body === '...' ? '…' : truncateLines(item.body, 8, w);
      return (
        <Box flexDirection="column">
          <Text color={item.ok ? HIPPO_MOSS : HIPPO_SOFT_RED}>{head}</Text>
          <Text dimColor>{body}</Text>
        </Box>
      );
    }
    case 'system':
      return <Text dimColor>{item.text}</Text>;
  }
};

/** Shorten a path for the top status bar: collapse $HOME to `~`, then
 *  if still too wide, replace the middle path components with `…` so
 *  the basename remains visible. */
function compactPath(p: string, maxLen: number): string {
  const home = process.env['HOME'];
  let s = home && p.startsWith(home) ? `~${p.slice(home.length)}` : p;
  if (s.length <= maxLen) return s;
  const parts = s.split('/');
  if (parts.length <= 2) return s.slice(0, maxLen - 1) + '…';
  // Keep first segment + last 2 (so the project + cwd remain visible).
  const head = parts[0] || '/';
  const tail = parts.slice(-2).join('/');
  const candidate = `${head}/…/${tail}`;
  return candidate.length <= maxLen ? candidate : `…/${tail}`.slice(-maxLen);
}

function statusGlyph(status: string): string {
  switch (status) {
    case 'running':
      return '◆';
    case 'completed':
      return '✓';
    case 'failed':
      return '✗';
    case 'killed':
      return '■';
    default:
      return '·';
  }
}

/** Render a seconds-since-X counter as a compact human-readable
 *  string. Used so a long dispatch reads "16m5s" instead of "(965s)"
 *  which the user mistook for postcommit-wait time. */
function formatElapsed(sec: number): string {
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  if (m < 60) return `${m}m${s}s`;
  const h = Math.floor(m / 60);
  return `${h}h${m % 60}m`;
}

/** Wrap long lines at the given width without dropping any content.
 *  Used for user + assistant messages where the full text matters
 *  (truncation would lose what the user typed or what the model
 *  reasoned). Terminal scrollback handles vertical overflow. */
function wrapToWidth(text: string, width: number): string {
  const out: string[] = [];
  for (const line of text.split('\n')) {
    if (line.length <= width) {
      out.push(line);
      continue;
    }
    // Greedy line wrap on character boundary - good enough for code
    // and prose. Word-aware wrapping breaks long URLs and identifiers.
    for (let i = 0; i < line.length; i += width) {
      out.push(line.slice(i, i + width));
    }
  }
  return out.join('\n');
}

function truncateLines(text: string, maxLines: number, width: number): string {
  const lines = text.split('\n');
  const head = lines
    .slice(0, maxLines)
    .map((l) => (l.length > width ? l.slice(0, width - 1) + '…' : l));
  if (lines.length > maxLines) head.push(`… (${lines.length - maxLines} more lines)`);
  return head.join('\n');
}

export async function runTui(
  flags: TuiFlags,
  sessionId: string,
  resume?: {
    startConv?: ChatMessage[];
    startRunningSummary?: string[];
    startTasks?: Task[];
    startTaskNextId?: number;
  },
): Promise<void> {
  // Print the hippo-purple ANSI logo before Ink boots so it lives in
  // the terminal scrollback above the live UI region. Done in stdout
  // (not stderr) because Ink writes to stdout; this keeps the logo
  // "above" Ink's draw area when the user scrolls back.
  const logo = await loadLogo();
  if (logo) process.stdout.write(logo);
  await new Promise<void>((resolve) => {
    const { waitUntilExit } = render(
      <App
        flags={flags}
        initialSessionId={sessionId}
        startConv={resume?.startConv}
        startRunningSummary={resume?.startRunningSummary}
        startTasks={resume?.startTasks}
        startTaskNextId={resume?.startTaskNextId}
      />,
    );
    void waitUntilExit().then(resolve);
  });
}
