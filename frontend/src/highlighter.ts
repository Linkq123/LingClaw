import hljs from 'highlight.js/lib/common';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import powershell from 'highlight.js/lib/languages/powershell';

// The common build covers the languages used most often in agent output while
// avoiding highlight.js' ~190-language bundle. Keep Windows and container
// snippets first-class because they are common in LingClaw workflows.
hljs.registerLanguage('dockerfile', dockerfile);
hljs.registerLanguage('powershell', powershell);

export default hljs;
