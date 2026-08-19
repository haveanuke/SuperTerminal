import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './components/App';
import { installTauriBridge } from './lib/tauri-bridge';
import './styles/global.css';

if ('__TAURI_INTERNALS__' in window) {
  installTauriBridge();
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>
);
