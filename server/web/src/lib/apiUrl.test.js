import test from 'node:test';
import assert from 'node:assert/strict';
import { buildQuery } from './apiUrl.js';

test('buildQuery 跳过空值，保留 0/false 并正确编码', () => {
  assert.equal(
    buildQuery({ key: '', page: 1, size: 0, flag: false, x: null, y: undefined, q: '中文 词' }),
    'page=1&size=0&flag=false&q=%E4%B8%AD%E6%96%87+%E8%AF%8D'
  );
  assert.equal(buildQuery({}), '');
});
