import '@fontsource/inter/latin-400.css';
import '@fontsource/inter/latin-500.css';
import '@fontsource/inter/latin-600.css';
import '@fontsource/jetbrains-mono/latin-500.css';
import './app.css';
import { mount } from 'svelte';
import App from './App.svelte';
import { inTauri } from './lib/api';
import { applyTheme, theme } from './lib/theme.svelte';

if (!inTauri) document.documentElement.dataset.mock = '';
applyTheme(theme.value);

const app = mount(App, { target: document.getElementById('app')! });

export default app;
