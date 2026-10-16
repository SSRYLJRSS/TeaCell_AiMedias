import process from 'node:process';
import { clearTimeout, setTimeout } from 'node:timers';
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync } from 'node:fs';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { buildPrototype } from './build.mjs';

const { WebSocket, fetch } = globalThis;

if (process.platform !== 'win32') throw new Error('A0_06_EDGE_BROWSER_CHECK_WINDOWS_ONLY');

const candidates = [
  process.env.MSEDGE_PATH,
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
].filter(Boolean);
const edge = candidates.find(path => existsSync(path));
if (!edge) throw new Error('MICROSOFT_EDGE_NOT_FOUND_SET_MSEDGE_PATH');

const htmlPath = await buildPrototype();
const html = await readFile(htmlPath);
const server = createServer((request, response) => {
  if (request.method !== 'GET' || request.url !== '/') {
    response.writeHead(404);
    response.end();
    return;
  }
  response.writeHead(200, {
    'Content-Type': 'text/html; charset=utf-8',
    'Content-Security-Policy': "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; base-uri 'none'; form-action 'none'",
  });
  response.end(html);
});
await new Promise((resolveListen, reject) => {
  server.once('error', reject);
  server.listen(0, '127.0.0.1', resolveListen);
});

const port = server.address().port;
const origin = `http://127.0.0.1:${port}`;
const profile = await mkdtemp(join(tmpdir(), 'tea-assistant-ui-edge-'));
const browser = spawn(edge, [
  '--headless=new',
  '--disable-gpu',
  '--disable-extensions',
  '--disable-background-networking',
  '--no-first-run',
  '--no-default-browser-check',
  '--remote-debugging-port=0',
  `--user-data-dir=${profile}`,
], { windowsHide: true, stdio: 'ignore' });

class DevTools {
  #socket;
  #nextId = 0;
  #pending = new Map();

  constructor(socket) {
    this.#socket = socket;
    socket.addEventListener('message', event => {
      const message = JSON.parse(String(event.data));
      if (!message.id) return;
      const pending = this.#pending.get(message.id);
      if (!pending) return;
      clearTimeout(pending.timer);
      this.#pending.delete(message.id);
      if (message.error) pending.reject(new Error(`${message.error.code}:${message.error.message}`));
      else pending.resolve(message.result ?? {});
    });
  }

  static async connect(url) {
    const socket = new WebSocket(url);
    await new Promise((resolveOpen, reject) => {
      socket.addEventListener('open', resolveOpen, { once: true });
      socket.addEventListener('error', () => reject(new Error('DEVTOOLS_SOCKET_OPEN_FAILED')), { once: true });
    });
    return new DevTools(socket);
  }

  send(method, params = {}) {
    const id = ++this.#nextId;
    return new Promise((resolveResult, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(id);
        reject(new Error(`DEVTOOLS_TIMEOUT:${method}`));
      }, 10000);
      this.#pending.set(id, { resolve: resolveResult, reject, timer });
      this.#socket.send(JSON.stringify({ id, method, params }));
    });
  }

  close() {
    this.#socket.close();
  }
}

const delay = milliseconds => new Promise(resolveDelay => setTimeout(resolveDelay, milliseconds));

async function waitForDevTools() {
  const activePort = join(profile, 'DevToolsActivePort');
  const started = Date.now();
  while (Date.now() - started < 30000) {
    if (browser.exitCode !== null) throw new Error(`EDGE_EXITED:${browser.exitCode}`);
    try {
      const [portLine] = (await readFile(activePort, 'utf8')).split(/\r?\n/);
      const devtoolsPort = Number(portLine);
      if (Number.isInteger(devtoolsPort) && devtoolsPort > 0) return devtoolsPort;
    } catch { /* The DevTools port file may not exist until Edge finishes starting. */ }
    await delay(100);
  }
  throw new Error('EDGE_DEVTOOLS_START_TIMEOUT');
}

async function waitForTarget(devtoolsPort) {
  const started = Date.now();
  while (Date.now() - started < 10000) {
    const response = await fetch(`http://127.0.0.1:${devtoolsPort}/json/list`);
    const targets = await response.json();
    const page = targets.find(target => target.type === 'page');
    if (page) return page;
    await delay(50);
  }
  throw new Error('EDGE_PAGE_TARGET_TIMEOUT');
}

let cdp;
let browserCdp;

async function evaluate(expression) {
  const response = await cdp.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
  if (response.exceptionDetails) {
    const detail = response.exceptionDetails.exception?.description ?? response.exceptionDetails.text ?? 'unknown';
    throw new Error(`PAGE_EVALUATION_FAILED:${detail}`);
  }
  return response.result?.value;
}

async function click(selector) {
  const target = JSON.stringify(selector);
  const point = await evaluate(`(() => {
    const element = document.querySelector(${target});
    if (!element) throw new Error('ELEMENT_NOT_FOUND:' + ${target});
    element.scrollIntoView({ block: 'center', inline: 'center' });
    const rect = element.getBoundingClientRect();
    const style = getComputedStyle(element);
    if (!rect.width || !rect.height || style.visibility === 'hidden' || style.display === 'none') {
      throw new Error('ELEMENT_NOT_VISIBLE:' + ${target});
    }
    const x = rect.left + rect.width / 2;
    const y = rect.top + rect.height / 2;
    const hit = document.elementFromPoint(x, y);
    if (!hit || !element.contains(hit)) throw new Error('CLICK_TARGET_OCCLUDED:' + ${target});
    return {
      x, y,
      target: element.outerHTML.slice(0, 180),
      hit: hit?.closest('button')?.outerHTML.slice(0, 180) ?? hit?.outerHTML.slice(0, 180) ?? '',
    };
  })()`);
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: point.x, y: point.y, pointerType: 'mouse' });
  await cdp.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: point.x, y: point.y, button: 'left', clickCount: 1, pointerType: 'mouse' });
  await cdp.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: point.x, y: point.y, button: 'left', clickCount: 1, pointerType: 'mouse' });
  await delay(30);
}

async function key(key, code, windowsVirtualKeyCode) {
  const data = { key, code, windowsVirtualKeyCode, nativeVirtualKeyCode: windowsVirtualKeyCode };
  await cdp.send('Input.dispatchKeyEvent', { type: 'rawKeyDown', ...data });
  await cdp.send('Input.dispatchKeyEvent', { type: 'keyUp', ...data });
  await delay(30);
}

function assert(condition, message) {
  if (!condition) throw new Error(`A0_06_BROWSER_ASSERTION:${message}`);
}

function assertContains(actual, expected, label) {
  assert(actual.includes(expected), `${label}: expected ${JSON.stringify(expected)} in ${JSON.stringify(actual)}`);
}

async function setViewport(width, height = 900) {
  await cdp.send('Emulation.setDeviceMetricsOverride', {
    width, height, deviceScaleFactor: 1, mobile: false,
  });
  await delay(100);
}

try {
  const devtoolsPort = await waitForDevTools();
  const target = await waitForTarget(devtoolsPort);
  cdp = await DevTools.connect(target.webSocketDebuggerUrl);
  const version = await (await fetch(`http://127.0.0.1:${devtoolsPort}/json/version`)).json();
  browserCdp = await DevTools.connect(version.webSocketDebuggerUrl);
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');
  await cdp.send('Page.navigate', { url: `${origin}/` });
  await setViewport(1366);
  const pageLoadStarted = Date.now();
  let pageReady = false;
  while (Date.now() - pageLoadStarted < 10000) {
    const ready = await evaluate(`location.href === ${JSON.stringify(`${origin}/`)} && document.readyState === 'complete' && document.querySelector('details > summary') !== null`);
    if (ready) {
      pageReady = true;
      break;
    }
    await delay(50);
  }
  assert(pageReady, '本机 A0-06 原型页面加载');

  const checks = [];
  await click('#guide');
  const guideText = await evaluate(`document.querySelector('#dialog').innerText`);
  assertContains(guideText, '欢迎来到茶馆', '打开首用引导');
  await click('[data-action="skip-guide"]');
  const guideStillOpen = await evaluate(`document.querySelector('#dialog').open`);
  assert(!guideStillOpen, '跳过首用引导后回到应用');
  checks.push('首次引导可跳过');

  await click('details > summary');
  await click('[data-scenario="switch"]');
  await click('[data-action="walk-next"]');
  let debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '附件：1', '旧对话添加附件');
  await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '右栏：import', '旧对话工作台');
  await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '当前对话：3', '新对话为空');
  assertContains(debug, '附件：0', '新对话附件隔离');
  await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '当前对话：1', '切回原对话');
  assertContains(debug, '附件：1', '切回后原附件保留');
  checks.push('鼠标点击切换对话并保留旧附件');

  await click('[data-scenario="handoff"]');
  await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '页面：import', '进入独立入库');
  await click('[data-action="handoff"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '页面：assistant', '清单交给助手');
  assertContains(debug, '附件：2', '清单附件已带入');
  await click('[data-action="prepare"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '计划：待审核', '生成待审核计划');
  await click('[data-action="approve"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '计划：已批准', '确认完整导入计划');
  assertContains(debug, '任务：1:执行中(0/2)', '产生演示任务');
  checks.push('独立入库按钮→助手→准备计划→确认任务');

  await click('[data-scenario="floating"]');
  for (let step = 0; step < 4; step += 1) await click('[data-action="walk-next"]');
  const floatingVisible = await evaluate(`document.querySelector('#floating .floating-chat') !== null`);
  assert(floatingVisible, '素材库打开悬浮助手');
  await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '页面：assistant', '悬浮助手转完整审核页');
  checks.push('素材库打开悬浮助手并转完整审核页');

  await click('[data-scenario="queue"]');
  for (let step = 0; step < 7; step += 1) await click('[data-action="walk-next"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '1:执行中(0/1)', '首个任务继续执行');
  assertContains(debug, '2:等待执行(0/1)', '第二个任务排队');
  await click('[data-cancel="2"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '2:已取消(0/1)', '取消排队任务');
  checks.push('第二任务排队，取消不影响第一个运行任务');

  await click('#app-mode');
  await click('#message');
  await cdp.send('Input.insertText', { text: 'A0-06 键盘草稿保留验证' });
  await click('[data-nav="import"]');
  debug = await evaluate(`document.querySelector('#debug').textContent`);
  assertContains(debug, '页面：import', '导航点击进入独立入库');
  await click('#home');
  const draft = await evaluate(`document.querySelector('#message').value`);
  assert(draft === 'A0-06 键盘草稿保留验证', `跨页草稿保留: ${JSON.stringify(draft)}`);
  checks.push('鼠标点击跨页导航并保留键盘输入的草稿');

  await click('[data-action="preview"]');
  await click('[data-action="search-assets"]');
  const searchSummary = await evaluate(`document.querySelector('.workbench').innerText`);
  assertContains(searchSummary, '2 个固定素材结果', '演示搜索固定素材');
  await click('[data-action="view-search"]');
  const searchView = await evaluate(`(() => {
    const page = document.querySelector('.page');
    return {
      badge: page.querySelector('.badge')?.textContent,
      ids: [...page.querySelectorAll('.asset .preview-shape')].map(item => item.textContent),
    };
  })()`);
  assert(searchView.badge === '本次搜索结果', `搜索结果标记进入素材库: ${JSON.stringify(searchView)}`);
  assert(JSON.stringify(searchView.ids) === '["1","3"]', `素材库仅显示固定搜索结果: ${JSON.stringify(searchView.ids)}`);
  checks.push('助手搜索结果按固定素材 ID 进入素材库');
  await click('#home');

  const widthBefore = Number(await evaluate(`document.querySelector('#separator').getAttribute('aria-valuenow')`));
  await click('#separator');
  await key('ArrowLeft', 'ArrowLeft', 37);
  const widthAfter = Number(await evaluate(`document.querySelector('#separator').getAttribute('aria-valuenow')`));
  assert(widthAfter === widthBefore + 20, `分隔条键盘调宽 ${widthBefore} -> ${widthAfter}`);
  checks.push('ArrowLeft 键盘调整右栏宽度');

  const layouts = [];
  for (const width of [960, 1366, 1920]) {
    await setViewport(width);
    const layout = await evaluate(`(() => {
      const logo = document.querySelector('.logo').getBoundingClientRect();
      const chat = document.querySelector('.chat').getBoundingClientRect();
      const history = document.querySelector('.history');
      return {
        viewportWidth: innerWidth,
        logoCenter: logo.left + logo.width / 2,
        chatWidth: chat.width,
        historyHidden: history?.classList.contains('auto-hidden') ?? false,
      };
    })()`);
    assert(Math.abs(layout.logoCenter - width / 2) <= 1, `Logo 中心位置 ${width}px: ${layout.logoCenter}`);
    assert(layout.chatWidth >= 300, `聊天区域最小宽度 ${width}px: ${layout.chatWidth}`);
    layouts.push(layout);
  }

  process.stdout.write(`${JSON.stringify({
    browser: 'Microsoft Edge headless',
    origin,
    checks: [...checks, '960/1366/1920 窗口下 Logo 居中且聊天区域不少于300px'],
    layouts,
  }, null, 2)}\n`);
} finally {
  try { await browserCdp?.send('Browser.close'); } catch { /* The browser may already have exited. */ }
  cdp?.close();
  browserCdp?.close();
  if (browser.exitCode === null) {
    await Promise.race([
      new Promise(resolveExit => browser.once('exit', resolveExit)),
      delay(5000),
    ]);
    if (browser.exitCode === null) browser.kill();
  }
  await new Promise(resolveClose => server.close(resolveClose));
  await rm(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 250 });
}
