import assert from 'node:assert/strict';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const root=resolve(dirname(fileURLToPath(import.meta.url)),'../..');
const source=readFileSync(resolve(root,'src/app/secure_chat_import.js'),'utf8');
const evidence=resolve(process.env.SECURE_CHAT_EVIDENCE || resolve(root,'.himind/reviews/secure-native-chat'));
mkdirSync(evidence,{recursive:true});
const browser=await chromium.launch({headless:true,channel:'chrome'});
const results=[];
try {
  for(const denied of [false,true]) {
    const page=await browser.newPage();
    page.on('pageerror',error=>process.stderr.write(error.message+'\n'));
    const key='andx_dev_'+'x'.repeat(43);
    const raw=JSON.stringify({schema:'andx.developer.access.v1',importProtocol:'himind.credential-import.v1',endpoint:'https://example.com',developerKey:key});
    const safe='[AndX 开发凭证已在本机安全导入；credential_ref=andx-native]';
    const imports=[];
    await page.route('http://localhost:21601/**',route=>{
      if(new URL(route.request().url()).pathname==='/himind/credentials/import') {
        imports.push(JSON.parse(route.request().postData()));
        return route.fulfill({status:denied?400:200,contentType:'application/json',body:JSON.stringify(denied?{error:'denied'}:{content:safe,imports:[{credential_ref:'cred-fixture'}]})});
      }
      return route.fulfill({contentType:'text/html; charset=utf-8',body:`<html><head><meta charset="utf-8"><script>window.__himindCredentialMatchers=["andx_dev_","andx.developer.access.v1","himind.credential-import.v1"];${source}</script></head><body><textarea aria-label="对话"></textarea><script>
      window.persisted=[];
      document.addEventListener('paste',event=>{window.persisted.push(event.clipboardData.getData('text/plain'));});
      document.querySelector('textarea').addEventListener('input',event=>{localStorage.setItem('draft',event.target.value);window.persisted.push(event.target.value);});
      </script></body></html>`});
    });
    await page.goto('http://localhost:21601/',{waitUntil:'domcontentloaded'});
    assert.ok((await page.content()).includes('aria-label="对话"'),await page.content());
    await page.getByRole('textbox',{name:'对话'}).focus();
    await page.evaluate(text=>{
      const transfer=new DataTransfer();transfer.setData('text/plain',text);
      document.activeElement.dispatchEvent(new ClipboardEvent('paste',{clipboardData:transfer,bubbles:true,cancelable:true}));
    },raw);
    await page.getByRole('status').filter({hasText:denied?'安全导入未完成':'接入凭证已安全导入'}).waitFor();
    const state=await page.evaluate(()=>({value:document.querySelector('textarea').value,stored:JSON.stringify({...localStorage}),persisted:window.persisted}));
    assert.equal(imports.length,1);assert.equal(imports[0].content,raw);
    assert.equal(state.value,denied?'':safe);
    assert.ok(!JSON.stringify(state).includes(key));
    assert.ok(!state.persisted.includes(raw));
    await page.evaluate(raw => {
      const target=document.querySelector('textarea');target.value=raw;
      target.dispatchEvent(new Event('input',{bubbles:true}));
    }, raw);
    assert.equal(await page.locator('textarea').inputValue(),'');
    assert.equal(await page.evaluate(raw => { try {localStorage.setItem('injected',raw);return false;} catch{return true;} },raw),true);
    const escaped='普通说明 {"importProtocol":"himind.credential\\u002dimport.v1","token":"private"}';
    assert.equal(await page.evaluate(raw => { try {localStorage.setItem('escaped',raw);return false;} catch{return true;} },escaped),true);
    results.push({mode:denied?'rejected':'imported',typed_guard:true,storage_guard:true,passed:true});
    await page.close();
  }
  writeFileSync(resolve(evidence,'report.json'),JSON.stringify({results},null,2));
  process.stdout.write(`PASS ${results.length} native chat import browser scenarios\n`);
} finally {await browser.close();}
