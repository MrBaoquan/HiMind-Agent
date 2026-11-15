// Run retained supplementary scenarios without executing the two human-deferred
// areas. Original producers stay byte-for-byte unchanged. Generated copies and
// exact excluded spans are reviewable in this task's host-owned runtime.
import assert from 'node:assert/strict';
import {readFileSync,writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createRuntime,workspace} from './service-ui-runtime.mjs';
const kind=process.argv[2];assert.ok(['budget','service'].includes(kind));
const runtime=createRuntime('retained');
const base=resolve(workspace,'frontend/scripts/remediation');
const records=[];
const hash=value=>createHash('sha256').update(value).digest('hex');
function generate(name,transform) {
  const original=readFileSync(resolve(base,name),'utf8');let text=transform(original);
  for(const dep of ['vite','@vitejs/plugin-react','playwright']) text=text.replaceAll(`from '${dep}'`,`from '${import.meta.resolve(dep)}'`).replaceAll(`import('${dep}')`,`import('${import.meta.resolve(dep)}')`);
  text=text.replaceAll("from './service-ui-runtime.mjs'",`from '${pathToFileURL(resolve(base,'service-ui-runtime.mjs')).href}'`);
  text=text.replaceAll("const root = resolve(dirname(fileURLToPath(import.meta.url)), '../..');",`const root = ${JSON.stringify(resolve(workspace,'frontend'))};`);
  const output=resolve(runtime.directory,name);writeFileSync(output,text);
  records.push({source:'frontend/scripts/remediation/'+name,source_sha256:hash(original),generated:output,generated_sha256:hash(text)});
  return pathToFileURL(output).href;
}
function omit(text,start,end,replacement) {
  const first=text.indexOf(start),last=text.indexOf(end,first);
  assert.ok(first>=0 && last>first);const excluded=text.slice(first,last+end.length);
  records.push({classification:'user_deferred/all_200_percent_zoom; no other assertion removal',excluded});
  return text.slice(0,first)+replacement+text.slice(last+end.length);
}
let entry;
if(kind==='budget') entry=generate('qa-budget-browser.mjs',text=>{
  text=omit(text,' // CSS zoom renders text',"await page.evaluate(()=>document.documentElement.style.zoom='');",' const zoomState={status:"user_deferred",passed:null};\n ');
  return text.replace('200% CSS zoom and equivalent layout','ordinary 640px layout; all 200% zoom user_deferred').replace('partial-200-percent-layout','partial-640-layout');
});
else {
  const scenarios=generate('service-ui-scenarios.mjs',text=>omit(text,"  await page.evaluate(() => { document.body.style.zoom = '2'; });","  await page.evaluate(() => { document.body.style.zoom = '1'; });",'  // all 200% zoom user_deferred\n'));
  entry=generate('service-ui-browser.mjs',text=>{
    text=omit(text,"  await page.setViewportSize({width:880,height:900});await page.evaluate(()=>document.body.style.zoom='2');","await page.evaluate(()=>document.body.style.zoom='1');",'  await page.setViewportSize({width:880,height:900});\n');
    return text.replace("from './service-ui-scenarios.mjs'",`from '${scenarios}'`);
  });
}
writeFileSync(resolve(runtime.directory,'generated-harness.json'),JSON.stringify({kind,records,user_deferred:{screen_reader_speech:{passed:null},all_200_percent_zoom:{passed:null}}},null,2));
await import(entry);
