import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach } from 'vitest';

// jsdom has no layout, but the virtualized grid and a few components ask for sizes
class RO { observe() {} unobserve() {} disconnect() {} }
(globalThis as unknown as { ResizeObserver: typeof RO }).ResizeObserver ??= RO;
Element.prototype.scrollIntoView ??= () => {};

afterEach(() => {
  cleanup();
  sessionStorage.clear();
  window.history.replaceState(null, '', '/');
});
