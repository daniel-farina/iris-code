// Smoke tests for DEFAULT_SYSTEM_PROMPT. We don't pin its exact text
// (that'd be a churn nightmare), but we do verify that load-bearing
// anchors are present so refactors don't silently delete them.

import { describe, it, expect } from 'vitest';
import { DEFAULT_SYSTEM_PROMPT } from '../src/system_prompt.js';

describe('DEFAULT_SYSTEM_PROMPT', () => {
  it('is non-empty', () => {
    expect(DEFAULT_SYSTEM_PROMPT.length).toBeGreaterThan(100);
  });

  it('references the locate-then-edit workflow tools (read/search/edit)', () => {
    // Not all 10 tools need to appear by name - the model also gets the
    // full ToolSpec list at request time. But these three are explicitly
    // named in the prompt's workflow guidance and must stay.
    const must = ['read', 'search', 'edit'];
    for (const name of must) {
      expect(DEFAULT_SYSTEM_PROMPT.toLowerCase()).toContain(name);
    }
  });

  it('contains the surgical-edit anchor', () => {
    expect(DEFAULT_SYSTEM_PROMPT.toLowerCase()).toMatch(/surgical/);
  });

  it('contains anti-pattern guidance so the model knows what NOT to do', () => {
    expect(DEFAULT_SYSTEM_PROMPT.toLowerCase()).toMatch(/anti.?patterns?/);
  });

  it('contains a "locate then edit" workflow hint', () => {
    expect(DEFAULT_SYSTEM_PROMPT.toLowerCase()).toMatch(/locate|find.*before|search.*before/);
  });

  it('warns against giant single-bash heredocs (the qwen3 422 mitigation)', () => {
    const lc = DEFAULT_SYSTEM_PROMPT.toLowerCase();
    expect(lc).toMatch(/one bash call per file|heredoc|file creation/);
  });

  it('contains modularity guidance', () => {
    expect(DEFAULT_SYSTEM_PROMPT.toLowerCase()).toMatch(/modular|composition|one responsibility/);
  });
});
