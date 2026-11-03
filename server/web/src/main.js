import './app.css';
import './lib/layout.css';
import { initDevice } from './lib/device.js';
import App from './App.svelte';

initDevice();

const app = new App({
  target: document.getElementById('app'),
});

export default app;
