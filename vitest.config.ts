import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    // The test-game/ tree is a separate playground project with its own
    // bun:test runner. Don't let vitest pick those files up.
    exclude: ['node_modules/**', 'dist/**', 'test-game/**'],
  },
});
