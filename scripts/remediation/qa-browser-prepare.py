from pathlib import Path
p=Path('frontend/scripts/check-service-ui.browser.mjs')
s=p.read_text(encoding='utf-8')
s=s.replace("'./remediation/service-ui-", "'./service-ui-")
s=s.replace("fileURLToPath(import.meta.url)), '..'", "fileURLToPath(import.meta.url)), '../..'")
preamble='<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>'
s=s.replace('<body><div id=', '<body>'+preamble+'<div id=')
Path('frontend/scripts/remediation/qa-browser.mjs').write_text(s,encoding='utf-8')
