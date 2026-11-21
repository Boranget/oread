import test from 'node:test';
import assert from 'node:assert/strict';
import { isEmptyChapter, isVolumeTitle } from './readerContent.js';

test('空正文/纯空白/仅标签都判定为空', () => {
  assert.equal(isEmptyChapter(null), false);
  assert.equal(isEmptyChapter({ content: '' }), true);
  assert.equal(isEmptyChapter({ content: '   \n\t' }), true);
  assert.equal(isEmptyChapter({ content: '<p></p><br>' }), true);
  assert.equal(isEmptyChapter({ content: '&nbsp;' }), true);
  assert.equal(isEmptyChapter({ content: '<p>正文</p>' }), false);
});

test('图片章节按图片数量判定', () => {
  assert.equal(isEmptyChapter({ content_type: 'image', images: [] }), true);
  assert.equal(isEmptyChapter({ content_type: 'image', images: undefined }), true);
  assert.equal(isEmptyChapter({ content_type: 'image', images: [{ url: 'a.jpg' }] }), false);
});

test('分卷标题识别（不误伤正文章节）', () => {
  for (const t of ['第2卷', '第一卷', '卷 12', 'Vol.3', 'Volume 2']) {
    assert.equal(isVolumeTitle(t), true, t);
  }
  for (const t of ['第一章', '第1话', '第二十章', '序章', '第一章 起始', '']) {
    assert.equal(isVolumeTitle(t), false, t);
  }
});
