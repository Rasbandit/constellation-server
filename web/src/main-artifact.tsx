import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { MemoryRouter } from 'react-router-dom';
import { installBrowserMock } from '../mock/browser';
import { App } from './App';
import { ToastProvider } from './components/ui';
import './styles.css';

// The artifact build: no dev server, so the stub API runs in the page, and routes live in memory
// (an artifact's address cannot carry paths).
installBrowserMock();

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <MemoryRouter>
      <ToastProvider>
        <App />
      </ToastProvider>
    </MemoryRouter>
  </StrictMode>,
);
