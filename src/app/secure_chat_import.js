(() => {
  if (window.__himindSecureImport) return;
  window.__himindSecureImport = true;
  // Injected by the host from installed extension declarations, before runtime
  // scripts execute. No business names or key formats live in this adapter.
  const markers = window.__himindCredentialMatchers || [];
  const sensitive = value => {
    if (typeof value !== 'string') return false;
    const decoded = value.replace(/\\u([\da-f]{4})/gi, (_match, hex) => String.fromCharCode(Number.parseInt(hex, 16)));
    return markers.some(marker => value.includes(marker) || decoded.includes(marker));
  };
  const notice = message => {
    const el = document.createElement('div');
    el.setAttribute('role', 'status');
    el.dataset.himindSecureImport = 'status';
    Object.assign(el.style, {position:'fixed',bottom:'20px',left:'20px',zIndex:'2147483647',maxWidth:'440px',padding:'12px',background:'#17212f',color:'#fff',borderRadius:'8px'});
    el.textContent = message;
    document.body.append(el);
    setTimeout(() => el.remove(), 7000);
  };
  let busy = false;
  document.addEventListener('paste', event => {
    const content = event.clipboardData?.getData('text/plain') || '';
    if (!sensitive(content)) return;
    const target = event.target instanceof Element ? event.target.closest('textarea,input,[contenteditable="true"]') : null;
    if (!target) return;
    event.preventDefault(); event.stopImmediatePropagation();
    if (busy) return;
    busy = true;
    notice('正在本机安全导入接入凭证…');
    void (async () => {
      try {
        const response = await fetch('/himind/credentials/import', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({content})});
        if (!response.ok) throw new Error();
        const result = await response.json();
        if (typeof result.content !== 'string' || sensitive(result.content) || !Array.isArray(result.imports) || result.imports.length === 0) throw new Error();
        target.focus();
        if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement) {
          const value = target.value, start = target.selectionStart ?? value.length, end = target.selectionEnd ?? start;
          const prototype = target instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
          Object.getOwnPropertyDescriptor(prototype, 'value').set.call(target, value.slice(0,start) + result.content + value.slice(end));
          target.dispatchEvent(new Event('input',{bubbles:true}));
        } else if (!document.execCommand('insertText',false,result.content)) throw new Error();
        notice('接入凭证已安全导入，发送消息即可继续。');
      } catch { notice('安全导入未完成，凭证未写入对话。请检查扩展、授权和网络后重试。'); }
      finally { busy = false; }
    })();
  }, true);
  // Capture typed input before framework draft listeners. The transport guard
  // also rejects programmatic mutations and clients bypassing these UI hooks.
  document.addEventListener('input', event => {
    const target = event.target;
    const value = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement ? target.value : target?.textContent;
    if (!sensitive(value)) return;
    event.stopImmediatePropagation();
    if ('value' in target) target.value = ''; else target.textContent = '';
    notice('检测到未导入凭证，草稿已清除。请粘贴后台复制的完整接入信息进行安全导入。');
  }, true);
  const nativeSetItem = Storage.prototype.setItem;
  Storage.prototype.setItem = function(key, value) {
    if (sensitive(String(value))) throw new Error('secure credential import required');
    return nativeSetItem.call(this, key, value);
  };
})();
