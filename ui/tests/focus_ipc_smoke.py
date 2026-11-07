"""Frontend concurrency/error regression. Native HWND/SendInput is tested separately."""
import json
import os
from pathlib import Path
from playwright.sync_api import expect, sync_playwright

MOCK = r'''
import {mockIPC, mockWindows} from '/node_modules/@tauri-apps/api/mocks.js';
import {emit} from '/node_modules/@tauri-apps/api/event.js';
mockWindows('main');
window.qaCalls=[];
window.qaPending=[];
mockIPC(async (cmd,args={}) => {
  window.qaCalls.push({cmd,args});
  if(cmd==='list_groups') return [];
  if(cmd==='list_clips') return [{id:1,kind:'text',contentType:'text',preview:'焦点回归测试记录',size:24,
    truncated:false,width:null,height:null,favorite:false,pinned:false,updatedAt:Date.now(),source:null,groupId:null,thumb:null}];
  if(cmd==='get_settings') return {settings:{theme:'light',keepOnBlur:false}};
  if(cmd==='get_capture_paused') return false;
  if(cmd==='get_rename_safe_mode') return false;
  if(cmd==='get_update_state') return {kind:'idle'};
  if(cmd==='plugin:app|version') return '0.2.1';
  if(cmd==='paste_clip'||cmd==='copy_clip') return new Promise((resolve,reject)=>window.qaPending.push({resolve,reject}));
  if(cmd==='hide_window') { await emit('window-hidden'); return; }
  return null;
},{shouldMockEvents:true});
'''

with sync_playwright() as p:
    browser = p.chromium.launch(headless=True, executable_path=os.environ.get('CLIPKEEP_QA_BROWSER_EXE'))
    page = browser.new_page(viewport={'width': 400, 'height': 600})
    errors=[]
    page.on('pageerror', lambda error: errors.append(str(error)))
    def intercept(route):
        response=route.fetch()
        route.fulfill(response=response, body=MOCK+'\n'+response.text())
    page.route('**/src/main.ts*', intercept)
    page.goto('http://127.0.0.1:1420/')
    item=page.get_by_text('焦点回归测试记录', exact=True)
    expect(item).to_be_visible()
    def count(command):
        return page.evaluate('(cmd)=>qaCalls.filter(c=>c.cmd===cmd).length',command)
    def event(name,payload=None):
        page.evaluate('async ([name,payload])=>(await import("/node_modules/@tauri-apps/api/event.js")).emit(name,payload)',[name,payload])

    item.click()
    expect(page.get_by_role('status')).to_have_text('正在准备内容…')
    item.click()
    page.keyboard.press('Enter')
    page.keyboard.press('Control+1')
    assert count('paste_clip')==1
    page.evaluate('qaPending[0].reject("准备失败，请重试")')
    expect(page.get_by_role('status')).to_have_text('准备失败，请重试')
    # Retry works once the failed request is finished.
    item.click()
    assert count('paste_clip')==2
    event('window-hidden')
    page.evaluate('qaPending[1].reject("原输入框已失效，请手动粘贴")')
    event('window-shown')
    expect(page.get_by_role('status')).to_have_text('原输入框已失效，请手动粘贴')

    # A late result from an older show must not clear the new request's busy guard.
    item.click()
    assert count('paste_clip')==3
    event('window-hidden')
    event('window-shown')
    item.click()
    assert count('paste_clip')==4
    page.evaluate('qaPending[2].reject("旧会话失败")')
    page.keyboard.press('Enter')
    assert count('paste_clip')==4
    expect(page.get_by_role('status')).to_have_text('正在准备内容…')
    page.evaluate('qaPending[3].resolve()')
    expect(page.get_by_text('正在准备内容…',exact=True)).to_have_count(0)

    page.keyboard.press('Shift+Enter')
    assert count('copy_clip')==1
    event('window-hidden')
    page.evaluate('qaPending[4].resolve()')
    event('window-shown')

    # Existing Escape behavior: clear search first, dismiss second; menus back out first.
    search=page.locator('.search input')
    search.fill('焦点')
    page.keyboard.press('Escape')
    expect(search).to_have_value('')
    assert count('hide_window')==0
    page.get_by_role('button',name='选择分组',exact=True).click()
    page.keyboard.press('Escape')
    expect(page.locator('.group-picker')).to_have_count(0)
    assert count('hide_window')==0
    page.keyboard.press('Escape')
    assert count('hide_window')==1
    event('focus-error','焦点恢复失败，内容已复制')
    event('window-shown')
    expect(page.get_by_role('status')).to_have_text('焦点恢复失败，内容已复制')

    # Explorer inline rename keeps the OS keyboard focus; search remains inert until normal mode.
    event('window-shown',True)
    expect(search).to_be_disabled()
    expect(page.get_by_role('status')).to_contain_text('资源管理器正在重命名')
    event('window-shown',False)
    expect(search).to_be_enabled()
    expect(search).to_be_focused()
    out=Path('docs/focus-qa')
    out.mkdir(parents=True,exist_ok=True)
    page.screenshot(path=str(out/'focus-error-ui.png'))
    assert not errors, errors
    report={'result':'PASS','checks':['duplicate click/Enter/Ctrl+number suppressed','failure retry','hidden error shown next time',
       'stale request cannot release new busy guard','Shift+Enter copy-only','Escape clears search before hide',
       'Escape closes submenu before hide','native close error visible on next show',
       'rename-safe mode disables search and leaves normal-mode autofocus intact'],'pageErrors':errors}
    (out/'focus-ipc-smoke.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(report,ensure_ascii=False))
    browser.close()
