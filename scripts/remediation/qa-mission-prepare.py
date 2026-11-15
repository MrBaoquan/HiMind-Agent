from pathlib import Path
s=Path('frontend/scripts/remediation/mission-ui-clock-evidence-browser.mjs').read_text(encoding='utf-8')
needle="name:'mission-clock-transport-fixture', configureServer(vite)"
preamble='<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>'
s=s.replace(needle,"name:'mission-clock-transport-fixture', transformIndexHtml(html){return html.replace('<head>', '<head>'+"+repr(preamble)+");}, configureServer(vite)")
Path('frontend/scripts/remediation/qa-mission-browser.mjs').write_text(s,encoding='utf-8')
