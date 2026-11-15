import assert from 'node:assert/strict';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { createRuntime } from './service-ui-runtime.mjs';
import { checkServiceSummaryLayout, checkServiceUiExtended } from './service-ui-scenarios.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
if (process.env.SERVICE_UI_MEASURE_ONLY === '1') throw new Error('Measurement-only mode cannot satisfy the required browser scenarios.');
const runtime = createRuntime('browser');
const evidence = runtime.directory;
const { createServer } = await import('vite');
const { default: react } = await import('@vitejs/plugin-react');
const { chromium } = await import('playwright');
const fixture = `
import React,{useState} from 'react';import {createRoot} from 'react-dom/client';
import {AiConnectionsPage} from '/src/pages/AiConnectionsPage';import {ConfirmProvider} from '/src/components/ConfirmDialog';
import {agentApi} from '/src/services/agentApi';import '/styles.css';
const service={id:'fixture',display_name:'验收 AI 服务',base_url:'https://example.test/'+ 'long-path-'.repeat(15),protocol:'openai-chat',model:'test-model',models:['test-model'],image_model:'',image_adapter:'',media:{}};
async function saveService(){window.saves++;if(window.saveError)throw Error(window.saveError);await new Promise(resolve=>setTimeout(resolve,500))}
let catalogMode='ready';window.setCatalogMode=mode=>{catalogMode=mode};window.saves=0;window.syncs=0;
agentApi.getMediaProviderCatalog=async()=>{await new Promise(resolve=>setTimeout(resolve,150));if(catalogMode==='error')throw Error('network unavailable');if(catalogMode==='denied')throw Error('403 forbidden');return {providers:[{id:'fixture',has_api_key:true,health:{state:catalogMode==='partial'?'degraded':'ready'},capabilities:[]}]}};
agentApi.planAIClientSync=async()=>({sync_state:'conflict',blocked:false,detail:'conflict',desired_revision:'new'});agentApi.syncAIClient=async()=>{window.syncs++;return {restart_required:false}};
function App(){const [revision,setRevision]=useState(0);const [empty,setEmpty]=useState(false);const [extra,setExtra]=useState(false);const [syncState,setSyncState]=useState('conflict');window.setSyncState=setSyncState;window.setExtra=setExtra;window.setEmpty=setEmpty;window.refresh=()=>setRevision(value=>value+1);
const client={target:'codex',state:'imported',sync_state:syncState,client_detected:true,service:'custom:fixture',drift_detail:'字段差异：model 从 old-model 改为 test-model；不含凭据',models:[]};
const mediaOnly={...service,id:'media-only',display_name:'仅语音 AI 服务',model:'',models:[],media:{'audio.speech':{adapter:'openai-speech',model:'speech-test'}}};const data={managed:{available:false,reason:'not_authorized'},custom:{services:empty?[]:extra?[service,mediaOnly]:[service],capability_routes:{chat:[{service_id:'fixture',model:''}]},balances:{}},clients:{targets:[client,{...client}]}};
const noop=async()=>{};return <ConfirmProvider><main style={{padding:12,minWidth:0}}><AiConnectionsPage initialTab={new URLSearchParams(location.search).get('tab')||'services'} aiServices={data} aiServiceTemplates={null} targets={[{id:'codex',name:'Codex',state:'configured',detected:true,supports_auto_configure:true}]} acpProfiles={{profiles:[],local_providers:[]}} gatewayStatus={null} gatewayProfiles={[]} dashboardEnabled={false} identity={null} busyAction={null} testResult={null} onRefresh={()=>setRevision(revision+1)} onSaveAIService={saveService} onFetchModels={async()=>['test-model']} onFetchSavedModels={async()=>['test-model']} onFetchBalance={noop} onRemoveAIService={noop} onCloneAIService={async()=>service} onSetCapabilityRoutes={noop} onOpenAccount={noop} onApplyTarget={noop} onApplyAll={noop} onRemoveAll={noop} onRemoveTarget={noop} onOpenDirectory={noop} onTest={noop} onImportAIClient={noop} onRemoveAIClient={noop} onSetBindingMode={noop} onRestartGateway={noop} onStopGateway={noop} onSetGatewayPort={noop} onSaveAcpProfile={noop} onSetAcpProfileEnabled={noop} onRemoveAcpProfile={noop}/></main></ConfirmProvider>};
createRoot(document.getElementById('root')).render(<App/>);`;
const virtual = resolve(root, '__service-ui-fixture.tsx').replaceAll('\\', '/');
let server;
let browser;
let context;
let page;
let timeout;
const observations=[];
const errors=[];
async function shot(name){await page.screenshot({path:resolve(evidence,name+'.png'),fullPage:true,timeout:5000});}
async function overflow(){return page.evaluate(()=>({viewport:innerWidth,document:document.documentElement.scrollWidth,outside:[...document.querySelectorAll('main *')].filter(element=>element.getBoundingClientRect().right>innerWidth+1).slice(0,15).map(element=>({tag:element.tagName,class:element.className,right:element.getBoundingClientRect().right})),dialogs:[...document.querySelectorAll('dialog')].map(element=>({width:element.clientWidth,scroll:element.scrollWidth,height:element.clientHeight,scrollHeight:element.scrollHeight}))}));}
try {
 server = await createServer({root,configFile:false,cacheDir:runtime.cache,plugins:[react(),{name:'service-ui-test',resolveId(id){if(id==='/__service-ui-fixture.tsx'||id===virtual)return virtual},load(id){if(id===virtual)return fixture},configureServer(server){server.middlewares.use('/__service-ui',async(req,res,next)=>{if(req.url&&!req.url.startsWith('/?')&&req.url!=='/')return next();res.setHeader('Content-Type','text/html');res.end(await server.transformIndexHtml('/__service-ui','<!doctype html><html lang="zh"><body><script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script><div id="root"></div><script type="module" src="/__service-ui-fixture.tsx"></script></body></html>'))})}}],server:{host:'127.0.0.1',port:0}});
 await server.listen();
 // Use explicit short, task-owned paths: Playwright's generated profile prefix
 // exceeds Windows mkdtemp limits beneath the host's two-hash resource root.
 const profile = resolve(evidence, 'p');
 const artifacts = resolve(evidence, 'a');
 mkdirSync(profile);mkdirSync(artifacts);
 context = await chromium.launchPersistentContext(profile,{artifactsDir:artifacts,headless:true,channel:process.env.SERVICE_UI_BROWSER_CHANNEL || 'chrome',timeout:30000,viewport:{width:880,height:900}});
 browser = context.browser();
 await Promise.race([(async () => {
 writeFileSync(resolve(evidence,'browser-runtime.json'),JSON.stringify({version:browser.version(),channel:process.env.SERVICE_UI_BROWSER_CHANNEL || 'chrome',productionComponents:true,api:'mocked-test-data'},null,2));
 page=context.pages()[0] || await context.newPage();
 page.on('pageerror',error=>errors.push(error.message));
 const url='http://127.0.0.1:'+server.httpServer.address().port+'/__service-ui/';
 await page.goto(url);await page.locator('.ai-catalog-state[aria-busy="true"]').waitFor();await shot('catalog-loading');await page.getByText('健康目录已更新；',{exact:false}).waitFor();
 for(const width of [320,390,480,620,880]){
  await page.setViewportSize({width,height:900});await shot('services-'+width);observations.push({phase:'before-conditional-layout',width,metrics:await overflow()});
  await checkServiceSummaryLayout({page,evidence,phase:`default-${width}`});
  await page.getByRole('button',{name:'新增 AI 服务',exact:true}).click();await page.getByRole('dialog').waitFor();await shot('form-'+width);observations.push({phase:'before-conditional-form',width,metrics:await overflow(),grid:await page.locator('.ai-service-form-grid').first().evaluate(element=>getComputedStyle(element).gridTemplateColumns)});
  await page.keyboard.press('Escape');
 }
 writeFileSync(resolve(evidence,'browser-observations.json'),JSON.stringify({observations,errors},null,2));
  await page.setViewportSize({width:880,height:900});
  for(const tab of ['mcp','services','gateway','routes','acp']){await page.goto(url+'?tab='+tab);await page.locator('#ai-tab-'+tab+'[aria-selected="true"]').waitFor();assert.equal(await page.locator('#ai-panel-'+tab).count(),1);assert.equal(await page.getByRole('tabpanel').count(),1);for(const control of await page.getByRole('tab').all()){assert.equal(await page.locator('#'+await control.getAttribute('aria-controls')).count(),1)}}
  await page.getByRole('tab',{name:'运行环境',exact:true}).focus();await page.keyboard.press('Home');await page.waitForFunction(()=>document.activeElement.id==='ai-tab-mcp'&&document.activeElement.getAttribute('aria-selected')==='true');await page.keyboard.press('End');await page.waitForFunction(()=>document.activeElement.id==='ai-tab-acp');await page.keyboard.press('ArrowRight');await page.waitForFunction(()=>document.activeElement.id==='ai-tab-mcp');
  for(const state of ['needs_update','failed','conflict']){await page.evaluate(state=>window.setSyncState(state),state);await page.waitForTimeout(50);assert.match(await page.locator('.ai-overview').innerText(),/1 个工具可运行 · 1 个需行动/);await page.getByRole('button',{name:/待处理/}).click();assert.equal(await page.locator('.ai-tool-row').count(),1);await page.getByRole('button',{name:/已就绪/}).click();assert.equal(await page.locator('.ai-tool-row').count(),1);await shot('tool-'+state)}
  await page.getByRole('button',{name:/待处理/}).click();await shot('tool-snapshot');
  await page.locator('.ai-tool-row').getByRole('button').first().click();await page.getByRole('button',{name:'查看差异',exact:true}).click();await page.getByRole('dialog').waitFor();assert.match(await page.getByRole('dialog').innerText(),/字段差异/);await page.keyboard.press('Escape');assert.equal(await page.evaluate(()=>window.syncs),0);
  await page.getByRole('tab',{name:'AI 服务',exact:true}).click();await page.getByText('健康目录已更新；',{exact:false}).waitFor();
  const trigger=page.getByRole('button',{name:'新增 AI 服务',exact:true});await trigger.click();
  const dialog=page.getByRole('dialog');assert.equal(await dialog.evaluate(element=>element.matches(':modal')),true);
  const modalCdp=await page.context().newCDPSession(page);await modalCdp.send('Accessibility.enable');const modalTree=await modalCdp.send('Accessibility.getFullAXTree');assert.equal(modalTree.nodes.filter(node=>!node.ignored&&node.role?.value==='tab').length,0);writeFileSync(resolve(evidence,'modal-accessibility-tree.json'),JSON.stringify(modalTree,null,2));
  for(const key of ['Tab','Shift+Tab',...Array(20).fill('Tab')]){await page.keyboard.press(key);assert.equal(await page.evaluate(()=>!!document.activeElement.closest('dialog:modal')),true)}
  await dialog.getByRole('button',{name:'下一步'}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-display_name').evaluate(element=>element===document.activeElement&&element.getAttribute('aria-invalid')==='true'),true);
  await page.getByLabel('服务名称',{exact:true}).fill('验收 AI 服务');await dialog.getByRole('button',{name:'下一步'}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-api_key').evaluate(element=>element===document.activeElement),true);
  await page.getByLabel('API Key',{exact:true}).fill('fake-not-a-real-key');await page.getByLabel('Base URL',{exact:true}).fill('');await dialog.getByRole('button',{name:'下一步'}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-base_url').evaluate(element=>element===document.activeElement),true);
  await page.getByLabel('Base URL',{exact:true}).fill('https://example.test/v1');
  const category=page.locator('.ai-service-preset-tab').nth(1);await category.click();const radio=page.getByRole('radio').first();await radio.check();assert.equal(await radio.isChecked(),true);await radio.focus();await page.keyboard.press('ArrowRight');assert.equal(await page.getByRole('radio',{checked:true}).count(),1);assert.equal(await category.getAttribute('aria-pressed'),'true');await page.getByRole('button',{name:'手动配置',exact:true}).click();
  await page.getByLabel('服务名称',{exact:true}).fill('验收 AI 服务');await page.getByLabel('API Key',{exact:true}).fill('fake-key');await page.getByLabel('Base URL',{exact:true}).fill('https://example.test/v1');await dialog.getByRole('button',{name:'下一步'}).click();assert.match(await page.locator('#ai-service-modal-title').innerText(),/能力与模型/);assert.equal(await page.locator('[aria-current="step"]').count(),1);
  const capabilities=page.locator('.ai-capability-picker input');
  const requiredModels=['model','image_model','image_edit_model','video_model','speech_model','transcribe_model','vision_model'];
  for(let active=0;active<requiredModels.length;active++){for(let index=0;index<await capabilities.count();index++){await capabilities.nth(index).setChecked(index===active)}await page.locator('#ai-service-'+requiredModels[active]).fill('');await dialog.getByRole('button',{name:'下一步'}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-'+requiredModels[active]).evaluate(element=>element===document.activeElement&&element.getAttribute('aria-invalid')==='true'&&!!element.getAttribute('aria-describedby')),true)}
  for(let index=0;index<await capabilities.count();index++){await capabilities.nth(index).setChecked(index===0)}
  await page.getByLabel('对话默认模型',{exact:true}).fill('');await dialog.getByRole('button',{name:'下一步'}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-model').evaluate(element=>element===document.activeElement),true);await page.getByLabel('对话默认模型',{exact:true}).fill('test-model');await dialog.getByRole('button',{name:'下一步'}).click();
  await dialog.locator('select').last().selectOption('new-api');await dialog.getByRole('button',{name:'保存服务',exact:true}).click();await page.locator('#ai-service-error-link').click();assert.equal(await page.locator('#ai-service-balance_user_id').evaluate(element=>element===document.activeElement&&element.getAttribute('aria-invalid')==='true'),true);await page.getByLabel('New API User ID',{exact:true}).fill('fake-user');
  await page.getByLabel('服务 ID',{exact:true}).fill('fixture');await dialog.getByRole('button',{name:'保存服务',exact:true}).click();const nested=page.getByRole('dialog').last();await nested.getByRole('button',{name:'取消',exact:true}).waitFor();assert.equal(await page.locator('dialog:modal').count(),2);await page.keyboard.press('Tab');assert.equal(await nested.evaluate(element=>element.contains(document.activeElement)),true);await page.keyboard.press('Escape');assert.equal(await page.locator('dialog:modal').count(),1);assert.equal(await page.getByLabel('服务 ID',{exact:true}).inputValue(),'fixture');
  await dialog.getByRole('button',{name:'保存服务',exact:true}).click();await page.getByRole('button',{name:'覆盖保存',exact:true}).click();assert.equal(await page.getByRole('button',{name:'保存中...'}).isDisabled(),true);await page.keyboard.press('Escape');assert.equal(await page.locator('dialog:modal').count(),1);await dialog.waitFor({state:'detached'});assert.equal(await page.evaluate(()=>window.saves),1);assert.equal(await trigger.evaluate(element=>element===document.activeElement),true);
  for(const mode of ['error','denied','partial','ready']){await page.evaluate(mode=>{window.setCatalogMode(mode);window.refresh()},mode);await page.waitForTimeout(250);assert.equal(await page.locator('.ai-service-row').count(),1);await shot('catalog-'+mode);if(mode==='error'||mode==='denied'){assert.equal(await page.getByRole('button',{name:'重试健康目录'}).count(),1);assert.match(await page.locator('.ai-catalog-state').innerText(),/最近成功快照/);assert.match(await page.locator('.ai-service-row').innerText(),/健康未知/)}}
  await page.evaluate(()=>window.setEmpty(true));await page.getByText('暂无本机 AI 服务，',{exact:false}).waitFor();await shot('initial-empty');await page.evaluate(()=>window.setEmpty(false));await page.locator('.ai-service-search input').fill('no-match');await page.getByText('没有匹配的服务，',{exact:false}).waitFor();await shot('filtered-empty');await page.locator('.ai-service-search input').fill('');
  for(const width of [320,390,480,620,880]){
   await page.setViewportSize({width,height:900});await page.evaluate(()=>{for(const element of document.querySelectorAll('.ai-client-copy strong,.ai-overview-copy>span:last-child,.ai-section-heading span')){element.dataset.original??=element.textContent;element.textContent=element.dataset.original+'补充说明与完整上下文'.repeat(30).slice(0,Math.ceil(element.dataset.original.length/2))}});const metrics=await overflow();observations.push({phase:'expanded-copy-long-url',width,metrics});writeFileSync(resolve(evidence,'browser-observations.json'),JSON.stringify({observations,errors},null,2));await shot('expanded-'+width);assert.equal(metrics.document<=width,true);
   await checkServiceSummaryLayout({page,evidence,phase:`expanded-${width}`});
  }
  await page.setViewportSize({width:880,height:900});await page.evaluate(()=>document.body.style.zoom='2');await shot('zoom-200-services');observations.push({phase:'css-zoom-200',metrics:await overflow()});await trigger.focus();await page.keyboard.press('Enter');await dialog.waitFor();await shot('zoom-200-form');observations.push({phase:'css-zoom-200-form',metrics:await overflow()});await page.keyboard.press('Escape');await page.evaluate(()=>document.body.style.zoom='1');
  await page.emulateMedia({reducedMotion:'reduce',colorScheme:'dark'});await trigger.click();await shot('dark-reduced-motion');observations.push({phase:'dark-reduced-motion',metrics:await overflow(),motion:await page.locator('.ai-service-modal').evaluate(element=>({transition:getComputedStyle(element).transitionDuration,animation:getComputedStyle(element).animationDuration}))});await page.keyboard.press('Escape');await page.emulateMedia({colorScheme:'light'});
  const ax=await page.context().newCDPSession(page);await ax.send('Accessibility.enable');const tree=await ax.send('Accessibility.getFullAXTree');writeFileSync(resolve(evidence,'accessibility-tree.json'),JSON.stringify(tree,null,2));
  const colors=await page.evaluate(()=>{const selectors=['.ai-service-fact-line','.ai-client-copy strong','.ai-catalog-state','.btn-primary','.ai-tab.active'];return selectors.map(selector=>{const element=document.querySelector(selector);const style=getComputedStyle(element);return {selector,color:style.color,background:style.backgroundColor,fontSize:style.fontSize,outline:style.outlineColor}})});writeFileSync(resolve(evidence,'rendered-colors.json'),JSON.stringify(colors,null,2));
  await trigger.focus();
  const contrast=await page.evaluate(()=>{
   const parse=value=>{const parts=value.match(/[\d.]+/g).map(Number);return [parts[0],parts[1],parts[2],parts[3]??1]};
   const blend=(front,back)=>[...front.slice(0,3).map((value,index)=>value*front[3]+back[index]*(1-front[3])),1];
   const background=element=>{const ancestors=[];for(let current=element;current;current=current.parentElement)ancestors.push(current);return ancestors.reverse().reduce((color,current)=>blend(parse(getComputedStyle(current).backgroundColor),color),[255,255,255,1])};
   const luminance=color=>color.slice(0,3).map(value=>{const normalized=value/255;return normalized<=.04045?normalized/12.92:((normalized+.055)/1.055)**2.4}).reduce((sum,value,index)=>sum+value*[.2126,.7152,.0722][index],0);
   const ratio=(front,back)=>{const values=[luminance(front),luminance(back)].sort((left,right)=>right-left);return (values[0]+.05)/(values[1]+.05)};
   const text=['.ai-service-fact-line','.ai-client-copy strong','.ai-catalog-state','.btn-primary','.ai-tab.active','.ai-service-health'].map(selector=>{const element=document.querySelector(selector);const style=getComputedStyle(element),back=background(element),front=blend(parse(style.color),back);return {selector,foreground:front,compositedBackground:back,ratio:ratio(front,back),threshold:4.5}});
   const focused=document.activeElement,style=getComputedStyle(focused),back=background(focused.parentElement),front=parse(style.outlineColor);return {text,focus:{color:front,compositedBackground:back,ratio:ratio(front,back),threshold:3},targets:[...document.querySelectorAll('.ai-service-icon-btn,.ai-tab')].map(element=>({label:element.getAttribute('aria-label')||element.textContent,width:element.getBoundingClientRect().width,height:element.getBoundingClientRect().height}))};
  });writeFileSync(resolve(evidence,'contrast-and-targets.json'),JSON.stringify(contrast,null,2));for(const sample of contrast.text)assert.equal(sample.ratio>=sample.threshold,true,JSON.stringify(sample));assert.equal(contrast.focus.ratio>=3,true);for(const target of contrast.targets){assert.equal(target.width>=24&&target.height>=24,true,JSON.stringify(target))}
  writeFileSync(resolve(evidence,'browser-observations.json'),JSON.stringify({observations,errors},null,2));
  await checkServiceUiExtended({page,url,evidence});
  assert.deepEqual(errors,[]);
  writeFileSync(resolve(evidence,'browser-assertions.json'),JSON.stringify({passed:true,classification:'real-browser-production-components-with-mocked-api',checks:['five deep links and keyboard tabs','native nested modal focus/esc/restore','required-field error links','radio keyboard and pressed/current','save busy double-submit guard','health failure isolated from configuration','empty and filtered empty','extended-assertions.json'],errors},null,2));
 assert.deepEqual(errors,[]);
 console.log('Real browser rendering completed: '+evidence);
 })(),new Promise((_,reject)=>{timeout=setTimeout(()=>reject(new Error('Service UI browser deadline exceeded')),Number(process.env.SERVICE_UI_TIMEOUT_MS || 120000))})]);
}catch(error){if(page){await shot('failure').catch(()=>{});}writeFileSync(resolve(evidence,'browser-failure.txt'),String(error.stack)+'\n'+(page ? await page.locator('body').innerText({timeout:5000}).catch(()=>'Page unavailable') : 'Browser unavailable'));throw error}finally{clearTimeout(timeout);try{await context?.close()}finally{try{await server?.close()}finally{rmSync(resolve(evidence,'p'),{recursive:true,force:true,maxRetries:3,retryDelay:100});writeFileSync(resolve(evidence,'cleanup.json'),JSON.stringify({browserClosed:!browser?.isConnected(),serverClosed:!server?.httpServer?.listening,profileRemoved:true},null,2))}}}
