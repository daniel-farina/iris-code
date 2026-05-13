// OpenAI-compatible chat schema. Matches what MTPLX accepts/emits over
// its /v1/chat/completions endpoint with stream=true. Field names mirror
// hip's Rust ChatMessage and ToolCall structs so the wire format stays
// identical.

export type Role = 'system' | 'user' | 'assistant' | 'tool';

export interface FunctionCall {
  name: string;
  /** JSON-encoded arguments string (per OpenAI spec, not an object). */
  arguments: string;
}

export interface ToolCall {
  id: string;
  /** "function" - kept verbose for OpenAI parity. */
  type: 'function';
  function: FunctionCall;
}

export interface ChatMessage {
  role: Role;
  content: string | null;
  /** Populated on assistant messages that asked for tool calls. */
  tool_calls?: ToolCall[];
  /** Required on role=tool messages to thread back to the originating call. */
  tool_call_id?: string;
  /** Optional name (rare; mostly for role=tool replies). */
  name?: string;
}

export interface ToolSpec {
  type: 'function';
  function: {
    name: string;
    description?: string;
    parameters: Record<string, unknown>;
  };
}

export interface SSEDelta {
  content?: string;
  role?: Role;
  tool_calls?: Array<{
    index: number;
    id?: string;
    type?: 'function';
    function?: { name?: string; arguments?: string };
  }>;
}

export interface SSEChoice {
  index: number;
  delta: SSEDelta;
  finish_reason?: string;
}

export interface SSEChunk {
  id?: string;
  choices: SSEChoice[];
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
  };
  error?: unknown;
}

export interface CompletionResult {
  content: string;
  tool_calls: ToolCall[];
  finish_reason?: string;
  usage?: SSEChunk['usage'];
  ttftMs?: number;
  totalMs?: number;
}

export function systemMessage(text: string): ChatMessage {
  return { role: 'system', content: text };
}
export function userMessage(text: string): ChatMessage {
  return { role: 'user', content: text };
}
export function assistantText(text: string): ChatMessage {
  return { role: 'assistant', content: text };
}
export function assistantToolCalls(text: string, calls: ToolCall[]): ChatMessage {
  return { role: 'assistant', content: text || null, tool_calls: calls };
}
export function toolResultMessage(call_id: string, name: string, text: string): ChatMessage {
  return { role: 'tool', tool_call_id: call_id, name, content: text };
}
