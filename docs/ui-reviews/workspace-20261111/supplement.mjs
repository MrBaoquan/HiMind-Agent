import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const base = process.cwd();
const out = resolve(base,'target/workspace-ui-audit-20261111');
mkdirSync(out,{recursive:true});
const {createServer} = await import(pathToFileURL(resolve(base,'frontend/node_modules/vite/dist/node/index.js')));
const {default:react} = await import(pathToFileURL(resolve(base,'frontend/node_modules/@vitejs/plugin-react/dist/index.js')));
const {chromium} = await import(pathToFileURL(resolve(base,'target/session-history-playwright/node_modules/playwright/index.mjs')));
const fixtureUrl = '/@fs/' + encodeURI(resolve(out,'fixture.tsx').replaceAll('\\','/'));
const server = await createServer({root:resolve(base,'frontend'),configFile:false,plugins:[react(),{name:'workspace-review-fixture',configureServer(vite){vite.middlewares.use('/__workspace-review',async(req,res,next)=>{if(new URL(req.url||'/','http://local').pathname!=='/')return next();res.setHeader('Content-Type','text/html; charset=utf-8');res.end(await vite.transformIndexHtml('/__workspace-review','<!doctype html><html lang="zh-CN"><head><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="icon" href="data:,"></head><body><div id="root"></div><script type="module" src="'+fixtureUrl+'"></script></body></html>'));});}}],server:{host:'127.0.0.1',port:5195,fs:{allow:[base]}}});
await server.listen();
const browser = await chromium.launch({headless:true,executablePath:'C:/Program Files/Google/Chrome/Application/chrome.exe'});
const page = await browser.newPage({viewport:{width:1280,height:900}});
const origin='http://127.0.0.1:'+server.httpServer.address().port;
const report={scope:'Current source rendered with real components and synthetic data; no persistent mutations',errors:[],layouts:[],modal:{},states:{}};
page.on('pageerror',e=>{report.errors.push(e.message);console.error('PAGE',e.message);});
page.on('console',e=>{if(e.type()==='error')console.error('BROWSER',e.text());});
page.on('requestfailed',r=>console.error('REQUEST',r.url(),r.failure()));
async function open(scenario='normal'){const wrapper=page.viewportSize().width>=1024?'shell':'isolated';await page.goto(origin+'/__workspace-review?scenario='+scenario+'&wrapper='+wrapper);await page.locator('.workspace-registry-head').waitFor();if(scenario!=='registry-error')await page.locator('.workspace-registry-project-layout').waitFor();}
async function layout(label){return await page.evaluate(label=>{const metrics=s=>{const e=document.querySelector(s);if(!e)return null;const r=e.getBoundingClientRect(),c=getComputedStyle(e);return {x:r.x,y:r.y,width:r.width,height:r.height,scrollWidth:e.scrollWidth,clientWidth:e.clientWidth,fontSize:c.fontSize,color:c.color,background:c.backgroundColor,overflow:c.overflow};};return {label,viewport:innerWidth,documentWidth:document.documentElement.scrollWidth,bodyWidth:document.body.scrollWidth,detail:metrics('.workspace-registry-project-detail'),history:metrics('.workspace-session-history'),tools:metrics('.workspace-session-history-tools'),overflow:[...document.querySelectorAll('.workspace-registry *')].map(e=>({e,r:e.getBoundingClientRect()})).filter(x=>x.r.right>innerWidth+1&&x.r.width>0).slice(0,12).map(x=>({selector:x.e.className,tag:x.e.tagName,right:x.r.right,width:x.r.width})),smallText:[...document.querySelectorAll('.workspace-registry strong,.workspace-registry small,.workspace-registry code,.workspace-registry span')].filter(e=>e.getBoundingClientRect().height>0&&parseFloat(getComputedStyle(e).fontSize)<12).slice(0,15).map(e=>({selector:e.className||e.parentElement.className,size:getComputedStyle(e).fontSize,text:e.textContent.slice(0,35)}))};},label);}
try {
  const extra = {layouts:[],drawer:{},contrast:[],sources:[]};
  for (const width of [1024,768]) {
    await page.setViewportSize({width,height:900});
    await page.goto(origin+'/__workspace-review?wrapper=shell');
    await page.locator('.workspace-session-history-row').first().waitFor();
    extra.layouts.push(await layout('desktop-shell-'+width));
    await page.screenshot({path:resolve(out,'desktop-shell-'+width+'.png')});
  }
  await page.setViewportSize({width:1024,height:900});await open();
  await page.getByRole('button',{name:'新建项目',exact:true}).click();
  extra.drawer = await page.evaluate(()=>{
    const e=document.querySelector('.workspace-registry-editor-head button'),r=e.getBoundingClientRect(),hit=document.elementFromPoint(r.x+r.width/2,r.y+r.height/2);
    return {close:{x:r.x,y:r.y,width:r.width,height:r.height},hit:{tag:hit.tagName,class:hit.className,text:hit.textContent.slice(0,30)},editorZ:getComputedStyle(document.querySelector('.workspace-registry-editor')).zIndex,menuZ:getComputedStyle(document.querySelector('.app-menu-bar')).zIndex};
  });
  await page.getByRole('dialog').getByRole('button',{name:'取消',exact:true}).click();
  extra.contrast = await page.locator('.workspace-session-history-source-state').evaluateAll(es=>es.map(e=>({class:e.className,color:getComputedStyle(e).color,background:getComputedStyle(e).backgroundColor})));
  extra.text = await page.evaluate(()=>({header:document.querySelector('.workspace-registry-head p').textContent,headingFont:getComputedStyle(document.querySelector('.workspace-session-history-title h4')).fontSize,rowTitle:getComputedStyle(document.querySelector('.workspace-session-history-row-main strong')).fontSize,rowPath:getComputedStyle(document.querySelector('.workspace-session-history-row-main code')).fontSize,rowMetadata:getComputedStyle(document.querySelector('.workspace-session-history-row-meta')).fontSize}));
  const {createHash}=await import('node:crypto');
  for(const path of ['frontend/src/pages/WorkspaceRegistryPage.tsx','frontend/src/pages/WorkspaceSessionHistoryPanel.tsx','frontend/src/pages/WorkspaceSessionHistoryPanel.css','frontend/styles.css','src/session_history.rs'])extra.sources.push({path,sha256:createHash('sha256').update(readFileSync(resolve(base,path))).digest('hex')});
  extra.generatedAt=new Date().toISOString();writeFileSync(resolve(out,'supplement.json'),JSON.stringify(extra,null,2));console.log(JSON.stringify(extra,null,2));
} finally {await browser.close();await server.close();}