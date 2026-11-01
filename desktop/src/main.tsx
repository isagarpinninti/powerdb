import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles.css';
import { initTheme } from '@dash/theme';
import { ToastProvider, Spinner } from '@dash/ui';
import { AppProvider } from './app';
import { Shell } from './Shell';

initTheme();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ToastProvider>
      <AppProvider fallback={<div className="flex h-full items-center justify-center"><Spinner /></div>}>
        <Shell />
      </AppProvider>
    </ToastProvider>
  </StrictMode>,
);
