import test from 'node:test';
import assert from 'node:assert/strict';
import { clamp, clamp01, clampPos, isReadable, num, sibling } from './readerMath.js';

test('clamp / clamp01 边界', () => {
  assert.equal(clamp(5, 0, 3), 3);
  assert.equal(clamp(-1, 0, 3), 0);
  assert.equal(clamp01(0.4), 0.4);
  assert.equal(clamp01(-2), 0);
  assert.equal(clamp01(3), 1);
  assert.equal(clamp01('x'), 0);
});

test('num 仅接受有限数字', () => {
  assert.equal(num('12', 5), 12);
  assert.equal(num('', 5), 5);
  assert.equal(num(null, 5), 5);
  assert.equal(num(undefined, 5), 5);
  assert.equal(num('abc', 5), 5);
  assert.equal(num(Infinity, 5), 5);
});

test('章节索引与卷标题跳过', () => {
  const chapters = [{}, { is_volume: true }, {}, { is_volume: true }, {}];
  assert.equal(clampPos(-1, 5), 0);
  assert.equal(clampPos(9, 5), 4);
  assert.equal(clampPos(0, 0), 0);
  assert.equal(isReadable(0, chapters), true);
  assert.equal(isReadable(1, chapters), false);
  assert.equal(isReadable(-1, chapters), false);
  assert.equal(sibling(0, 1, chapters), 2);
  assert.equal(sibling(2, 1, chapters), 4);
  assert.equal(sibling(0, -1, chapters), -1);
  assert.equal(sibling(4, 1, chapters), -1);
  assert.equal(sibling(4, -1, chapters), 2);
});
