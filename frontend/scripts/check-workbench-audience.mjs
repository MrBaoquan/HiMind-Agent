import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createServer } from 'vite';
import react from '@vitejs/plugin-react';
import { chromium } from 'playwright';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const evidence = resolve(root, '../../.himind/long-tasks/20261120-extension-review-audience/evidence/browser');
mkdirSync(evidence, { recursive: true });
const fixture = `
import React from 'react';
import { createRoot } from 'react-dom/client';
import { WorkbenchAudienceDialog } from '/src/components/WorkbenchAudienceDialog';
import { createOrganizationDirectoryAdapter } from '/src/services/organizationDirectory';
import '/styles.css';
window.directoryCalls=[];
window.rejectNext=false;
const nodes=[
 {id:'dept-a',kind:'department',name:'研发中心',has_children:true,path:'研发中心'},
 {id:'dept-b',kind:'department',name:'产品设计与数字化展示交互体验研发部门',path:'产品设计与数字化展示交互体验研发部门'},
 {id:'u-a',kind:'person',name:'张三',parent_id:'dept-a',path:'研发中心'},
 {id:'u-b',kind:'person',name:'张三',parent_id:'dept-b',path:'产品设计与数字化展示交互体验研发部门'},
 {id:'u-c',kind:'person',name:'李四',parent_id:'dept-a',path:'研发中心'}
];
const paginated=new URLSearchParams(location.search).get('paging')==='1';
const namespace=new URLSearchParams(location.search).get('provider')==='memory'?'test-independent-directory':'connected-organization';
async function result(operation,value){
 window.directoryCalls.push({operation,value});
 if(window.rejectNext){window.rejectNext=false;throw Error('目录暂时不可用，请重试');}
 if(operation==='children')return {nodes:nodes.filter(node=>(node.parent_id||'')===(value||''))};
 if(operation==='search')return {nodes:nodes.filter(node=>(node.name+' '+node.path).includes(value))};
 return {nodes:nodes.filter(node=>value.includes(node.kind+':'+node.id))};
}
const directory=namespace==='test-independent-directory'?{namespace,
 children:(parent)=>result('children',parent),search:(query)=>result('search',query),resolve:(refs)=>result('resolve',refs)
}:createOrganizationDirectoryAdapter(async params=>{
 const value=key=>params.find(item=>item[0]===key)?.[1];
 const operation=value('operation');
 const page=await result(operation,operation==='children'?value('parent_id'):operation==='search'?value('query'):params.filter(item=>item[0]==='ref').map(item=>item[1]));
 if(paginated && operation==='children' && !value('parent_id')) {const cursor=Number(value('cursor')||0);return {nodes:page.nodes.slice(cursor,cursor+1),next_cursor:cursor+1<page.nodes.length?String(cursor+1):'',protocol:'organization-directory.v1',namespace};}
 return {...page,protocol:'organization-directory.v1',namespace};
},namespace);
const context={audience:{mode:'organization'},review_departments:[{id:'dept-a',kind:'department',name:'研发中心'}]};
const load=async()=>context;
function App(){const[saved,setSaved]=React.useState(null);const[open,setOpen]=React.useState(true);return <main>{open&&<WorkbenchAudienceDialog name='组织资产插件' version='1.2.0' directory={directory} load={load} onConfirm={async scope=>{window.saved=scope;setSaved(scope)}} onClose={()=>setOpen(false)}/>}<pre data-testid='saved'>{saved?JSON.stringify(saved):''}</pre></main>}
createRoot(document.getElementById('root')).render(<App/>);
`;
const virtual = resolve(root, '__audience-fixture.tsx').replaceAll('\\', '/');
const plugin = {
  name: 'audience-fixture',
  resolveId(id) { if (id === virtual || id === '/__audience-fixture.tsx') return virtual; },
  load(id) { if (id === virtual) return fixture; },
  configureServer(server) {
    server.middlewares.use('/__audience', async (_request, response) => {
      response.setHeader('Content-Type', 'text/html');
      const preamble = '<script type="module">import RefreshRuntime from "/@react-refresh";RefreshRuntime.injectIntoGlobalHook(window);window.$RefreshReg$=()=>{};window.$RefreshSig$=()=>type=>type;window.__vite_plugin_react_preamble_installed__=true;</script>';
      response.end(await server.transformIndexHtml('/__audience', `<!doctype html><html lang="zh"><meta name="viewport" content="width=device-width, initial-scale=1"><body>${preamble}<div id="root"></div><script type="module" src="/__audience-fixture.tsx"></script></body></html>`));
    });
  },
};
const server = await createServer({ root, configFile: false, cacheDir: resolve(evidence, 'vite-cache'), plugins: [react(), plugin], server: { host: '127.0.0.1', port: 0 } });
const passed = [];
let browser;
try {
  await server.listen();
  browser = await chromium.launch({ headless: true, ...(process.env.AUDIENCE_CHROMIUM ? { executablePath: process.env.AUDIENCE_CHROMIUM } : {}) });
  const page = await browser.newPage({ viewport: { width: 1000, height: 900 } });
  page.setDefaultTimeout(15000);
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  const url = `http://127.0.0.1:${server.httpServer.address().port}/__audience`;
  const dialog = () => page.getByRole('dialog');
  async function fresh(provider = 'protocol') {
    await page.goto(`${url}?provider=${provider}`, { waitUntil: 'domcontentloaded' });
    await dialog().getByText('组织内公开').first().waitFor();
  }
  await fresh();
  assert.deepEqual(await page.evaluate(() => window.directoryCalls), []);
  assert.equal(await dialog().getByLabel('搜索人员或部门').count(), 0);
  await page.screenshot({ path: resolve(evidence, 'default-public.png'), fullPage: true });
  await dialog().getByRole('button', { name: '提交审核' }).click();
  assert.deepEqual(await page.evaluate(() => window.saved.audience), { mode: 'organization' });
  passed.push('默认公开无需组织目录请求，可以直接提审');
  for (const provider of ['protocol', 'memory']) {
    await fresh(provider);
    await dialog().getByRole('button', { name: '修改', exact: true }).click();
    await dialog().getByRole('radio', { name: '指定人员和部门' }).check();
    await dialog().getByRole('button', { name: '浏览 研发中心', exact: true }).click();
    await dialog().getByRole('checkbox', { name: /李四/ }).check();
    await dialog().getByLabel('搜索人员或部门').fill('张三');
    await dialog().getByRole('checkbox', { name: /^张三 研发中心$/ }).check();
    assert.equal(await dialog().getByRole('checkbox', { name: /^张三 / }).count(), 2);
    await dialog().getByLabel('搜索人员或部门').fill('产品设计与数字化');
    await dialog().getByRole('checkbox', { name: /^产品设计与数字化展示交互体验研发部门 / }).check();
    assert.match(await dialog().locator('footer').innerText(), /2 人 · 1 个部门/);
    await dialog().getByLabel('搜索人员或部门').fill('');
    await dialog().getByRole('checkbox', { name: /李四/ }).waitFor();
    if (provider === 'protocol') {
      await page.screenshot({ path: resolve(evidence, 'restricted-organization.png'), fullPage: true });
      await page.setViewportSize({ width: 480, height: 900 });
      assert.equal(await dialog().evaluate(element => element.scrollWidth <= element.clientWidth + 1), true);
      await page.screenshot({ path: resolve(evidence, 'restricted-narrow.png'), fullPage: true });
      await page.keyboard.press('Tab');
      assert.equal(await page.evaluate(() => !!document.activeElement?.closest('dialog')), true);
    }
    await dialog().getByRole('button', { name: '提交审核' }).click();
    assert.deepEqual(await page.evaluate(() => window.saved.audience.user_ids.slice().sort()), ['u-a', 'u-c']);
    assert.deepEqual(await page.evaluate(() => window.saved.audience.department_ids), ['dept-b']);
    passed.push(`${provider}: 同一组件浏览层级、搜索同名成员、保留多选并提交`);
    await page.setViewportSize({ width: 1000, height: 900 });
  }
  await fresh();
  await dialog().getByRole('button', { name: '修改', exact: true }).click();
  await page.evaluate(() => { window.rejectNext = true; });
  await dialog().getByRole('radio', { name: '指定人员和部门' }).check();
  await dialog().getByRole('button', { name: '重试', exact: true }).click();
  await dialog().getByRole('checkbox', { name: /^研发中心 / }).check();
  await dialog().getByRole('button', { name: '清空', exact: true }).click();
  assert.equal(await dialog().getByRole('button', { name: '提交审核' }).isDisabled(), true);
  passed.push('目录失败可重试，清空后禁止提交空的定向范围');
  await page.goto(`${url}?paging=1`, { waitUntil: 'domcontentloaded' });
  await dialog().getByRole('button', { name: '修改', exact: true }).click();
  await dialog().getByRole('radio', { name: '指定人员和部门' }).check();
  await dialog().getByRole('checkbox', { name: /^研发中心 / }).check();
  await dialog().getByRole('button', { name: '加载更多', exact: true }).click();
  await dialog().getByRole('checkbox', { name: /^产品设计与数字化展示交互体验研发部门 / }).check();
  assert.match(await dialog().locator('footer').innerText(), /0 人 · 2 个部门/);
  await page.keyboard.press('Escape');
  assert.equal(await page.getByRole('dialog').count(), 0);
  passed.push('分页加载保持选择，Escape 关闭弹窗');
  assert.deepEqual(errors, []);
  writeFileSync(resolve(evidence, 'report.json'), JSON.stringify({ passed, pageErrors: errors }, null, 2));
  console.log(JSON.stringify({ passed, evidence }));
} finally {
  await browser?.close();
  await server.close();
}
