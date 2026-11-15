from pathlib import Path
s=Path('frontend/scripts/check-mission-budget.browser.mjs').read_text(encoding='utf-8').replace("'./remediation/service-ui-runtime.mjs'","'./service-ui-runtime.mjs'")
preamble='<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>'
s=s.replace("name:'isolated-real-mission-store',configureServer", "name:'isolated-real-mission-store',transformIndexHtml(html){return html.replace('<head>','<head>'+"+repr(preamble)+");},configureServer")
Path('frontend/scripts/remediation/qa-budget-browser.mjs').write_text(s,encoding='utf-8')
