import './app.css';
import './lib/layout.css';
import { initTheme } from './lib/theme.js';
import App from './App.svelte';

initTheme();

const app = new App({
  target: document.getElementById('app'),
});

export default app;
