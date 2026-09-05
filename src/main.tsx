import { createRoot } from 'react-dom/client'
import App from './App.tsx'
import './index.css'
import { initI18n } from './i18n'

// Awaited rather than imported for side effects: the active locale is now fetched as its own
// chunk, so rendering before it resolves would flash untranslated keys.
initI18n().finally(() => {
  createRoot(document.getElementById("root")!).render(<App />);
});
