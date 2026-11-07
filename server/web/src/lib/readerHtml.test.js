import test from 'node:test';
import assert from 'node:assert/strict';
import { absImg, htmlToParas, imgAttr, pickImgSrc, plainChars, textToParas } from './readerHtml.js';

test('plainChars 去掉标签与空白后计数', () => {
  assert.equal(plainChars(''), 0);
  assert.equal(plainChars('<p>你好 世界</p>'), 4);
  assert.equal(plainChars(null), 0);
});

test('imgAttr 兼容引号与裸值', () => {
  assert.equal(imgAttr('<img src="a.png">', 'src'), 'a.png');
  assert.equal(imgAttr("<img src='b.png'>", 'src'), 'b.png');
  assert.equal(imgAttr('<img src=c.png>', 'src'), 'c.png');
  assert.equal(imgAttr('<img>', 'src'), '');
});

test('pickImgSrc 懒加载优先，其次 src，最后 srcset', () => {
  assert.equal(pickImgSrc('<img src="placeholder.gif" data-src="real.jpg">'), 'real.jpg');
  assert.equal(pickImgSrc('<img src="cover.jpg">'), 'cover.jpg');
  assert.equal(pickImgSrc('<img srcset="one.jpg 1x, two.jpg 2x">'), 'one.jpg');
  assert.equal(pickImgSrc('<img>'), '');
});

test('absImg 只接受可解析的 http/https 地址', () => {
  assert.equal(absImg('pic.jpg', 'https://a.test/b/ch.html'), 'https://a.test/b/pic.jpg');
  assert.equal(absImg('//cdn.test/x.png', 'https://a.test/'), 'https://cdn.test/x.png');
  assert.equal(absImg('data:image/png;base64,AAA', 'https://a.test/'), '');
  assert.equal(absImg('javascript:void(0)', 'https://a.test/'), '');
  assert.equal(absImg('', 'https://a.test/'), '');
});

test('textToParas 处理块级标签与实体', () => {
  assert.deepEqual(textToParas('<p>A&amp;B</p><p>C&nbsp;D</p>'), ['A&B', 'C D']);
  assert.deepEqual(textToParas('a<br>b'), ['a', 'b']);
  assert.deepEqual(textToParas('   '), []);
});

test('htmlToParas 保留图片并切开文本，连续调用不受全局正则影响', () => {
  const html = '<p>开头</p><img src="a.jpg" alt="图"><p>结尾</p>';
  const first = htmlToParas(html, 'https://a.test/ch/1.html');
  assert.deepEqual(first, [
    { type: 'text', text: '开头' },
    { type: 'image', src: 'https://a.test/ch/a.jpg', alt: '图' },
    { type: 'text', text: '结尾' },
  ]);
  assert.deepEqual(htmlToParas(html, 'https://a.test/ch/1.html'), first);
});

test('htmlToParas 忽略无法归一的图片地址', () => {
  assert.deepEqual(htmlToParas('<img src="data:x"><p>正文</p>', 'https://a.test/'), [
    { type: 'text', text: '正文' },
  ]);
});
