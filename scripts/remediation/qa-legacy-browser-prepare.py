from pathlib import Path
s=Path('frontend/scripts/remediation/qa-mission-browser.mjs').read_text(encoding='utf-8')
insert='''
  const legacy = snapshot(); legacy.mission.status='waiting'; legacy.mission.tasks[0].status='awaiting_verification';
  legacy.mission.effective_wait={...legacy.mission.effective_wait,reason:'system_verification',actor:'system',can_resume:true};
  await load(legacy,'legacy-owned-pending');
  const legacyText=await page.locator('.mission-detail').innerText();
  writeFileSync(resolve(evidence,'legacy-dom.txt'),legacyText);await capture('legacy-owned-pending');
  assert.doesNotMatch(legacyText,/系统处理 · 无需你操作/,'legacy waiting requires explicit user resume');
'''
s=s.replace('  assert.deepEqual(pageErrors, []);',insert+'  assert.deepEqual(pageErrors, []);')
Path('frontend/scripts/remediation/qa-legacy-browser.mjs').write_text(s,encoding='utf-8')
