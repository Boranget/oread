import test from 'node:test';
import assert from 'node:assert/strict';
import { get } from 'svelte/store';
import { createImageLoader } from './readerImages.js';

const flush = () => new Promise((resolve) => setImmediate(resolve));

test('queue 只加载图片段，并对同一地址去重', async () => {
  const calls = [];
  const loader = createImageLoader({
    load: async (src) => {
      calls.push(src);
      return { objectUrl: `blob:${src}`, bytes: 3 };
    },
  });
  loader.queue([{ type: 'text', text: 'x' }, { type: 'image', src: 'a' }, { type: 'image', src: 'a' }]);
  assert.deepEqual(calls, ['a']);
  assert.equal(get(loader.state)['a'].status, 'loading');
  await flush();
  const rec = get(loader.state)['a'];
  assert.equal(rec.status, 'ok');
  assert.equal(rec.objectUrl, 'blob:a');
  assert.equal(rec.bytes, 3);
  // 已完成的不再重复请求
  loader.queue([{ type: 'image', src: 'a' }]);
  await flush();
  assert.deepEqual(calls, ['a']);
  loader.dispose();
});

test('加载失败进入 error，queue 不自动重试，retry 可恢复', async () => {
  let fail = true;
  let calls = 0;
  const loader = createImageLoader({
    load: async (src) => {
      calls += 1;
      if (fail) throw new Error('boom');
      return { objectUrl: `blob:${src}` };
    },
  });
  loader.queue([{ type: 'image', src: 'a' }]);
  await flush();
  assert.equal(get(loader.state)['a'].status, 'error');
  assert.match(get(loader.state)['a'].error, /boom/);
  loader.queue([{ type: 'image', src: 'a' }]);
  await flush();
  assert.equal(calls, 1);
  fail = false;
  loader.retry('a');
  await flush();
  assert.equal(get(loader.state)['a'].status, 'ok');
  assert.equal(get(loader.state)['a'].objectUrl, 'blob:a');
  loader.dispose();
});

test('markError 直接标记解码失败', () => {
  const loader = createImageLoader({ load: async () => ({}) });
  loader.markError('a');
  assert.equal(get(loader.state)['a'].status, 'error');
  assert.equal(get(loader.state)['a'].error, '图片解码失败，请重试');
  loader.dispose();
});

test('超出 maxCache 按插入顺序淘汰并释放 objectURL', async () => {
  const revoked = [];
  const original = URL.revokeObjectURL;
  URL.revokeObjectURL = (u) => revoked.push(u);
  try {
    const loader = createImageLoader({
      maxCache: 2,
      load: async (src) => ({ objectUrl: `blob:${src}` }),
    });
    loader.queue([{ type: 'image', src: 'a' }, { type: 'image', src: 'b' }, { type: 'image', src: 'c' }]);
    await flush();
    assert.deepEqual(revoked, ['blob:a']);
    assert.deepEqual(Object.keys(get(loader.state)), ['b', 'c']);
    loader.dispose();
    assert.deepEqual(revoked, ['blob:a', 'blob:b', 'blob:c']);
    assert.deepEqual(get(loader.state), {});
  } finally {
    URL.revokeObjectURL = original;
  }
});
