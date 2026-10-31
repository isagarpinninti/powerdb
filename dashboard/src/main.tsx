import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import { App } from './App';
import { SessionProvider } from './session';
import { initTheme } from './theme';
import { ToastProvider } from './ui';

initTheme(); // before the first render, so the page never flashes the wrong theme

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ToastProvider>
      <SessionProvider>
        <App />
      </SessionProvider>
    </ToastProvider>
  </StrictMode>,
);
