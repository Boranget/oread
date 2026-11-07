import test from 'node:test';
import assert from 'node:assert/strict';
import { describeFailure, describeSourceError, errorText, shortText } from './apiErrors.js';

test('describeSourceError 覆盖常见上游报错形态', () => {
  assert.equal(describeSourceError(''), '未知错误');
  assert.match(describeSourceError('builder error: invalid header'), /请求无法构造/);
  assert.equal(describeSourceError('operation timeout'), '书源站点响应超时');
  assert.match(describeSourceError('error sending request: connection refused'), /无法连接书源站点/);
  assert.equal(describeSourceError('internal: 解析失败'), '书源解析失败：解析失败');
  // 现有正则要求 HTTP 与状态码紧邻，带空格时走原文兜底
  assert.equal(describeSourceError('HTTP 503'), 'HTTP 503');
  assert.equal(describeSourceError('HTTP503'), 'HTTP503');
  assert.equal(describeSourceError('status=429'), '书源站点返回 HTTP 429');
  assert.equal(describeSourceError('<b>普通</b>  错误'), '普通 错误');
});

test('shortText 去标签、压空白并截断', () => {
  assert.equal(shortText('<p>a</p>\n  b'), 'a b');
  const long = 'x'.repeat(200);
  assert.equal(shortText(long).length, 161);
  assert.ok(shortText(long).endsWith('…'));
});

test('describeFailure 按状态码与关键词给出可操作提示', () => {
  assert.equal(describeFailure({ status: 401 }, '搜索'), '搜索失败：登录已失效。请重新登录后再试。');
  assert.match(describeFailure({ status: 403 }, '发现'), /没有权限/);
  assert.match(describeFailure({ status: 0, message: '请求超时' }, '搜索'), /请求超时/);
  assert.match(describeFailure({ status: 0, message: '其他' }, '搜索'), /连不上服务器/);
  assert.match(describeFailure({ status: 504 }, '书内搜索'), /请求超时/);
  assert.match(describeFailure({ status: 500 }, '读取目录'), /服务端出错（HTTP 500）/);
  assert.match(describeFailure({ status: 400, message: 'missing_book_url' }, '加载正文'), /缺少书籍地址/);
  assert.match(describeFailure({ status: 400, message: '目录为空' }, '读取目录'), /没有返回章节/);
  assert.equal(
    describeFailure({ status: 422, message: '字体越界' }, '设置'),
    '设置失败：字体越界。请按提示修改后重试。'
  );
  assert.match(describeFailure({}, '操作'), /操作失败：未知错误/);
});

test('errorText 统一 400/422、401、404、5xx 与稳定错误码', () => {
  assert.equal(errorText(400, 'source not found'), '书源不存在');
  assert.equal(errorText(400, 'internal: boom'), '书源抓取失败（boom）');
  assert.equal(errorText(400, { message: '字体越界' }), '字体越界');
  assert.equal(errorText(400, 'missing field `name`'), '缺少参数：name');
  assert.equal(errorText(400, 'invalid type: string'), '请求参数格式不正确');
  assert.equal(errorText(422, 'Expected request with `Content-Type: application/json`'), '请求体不是合法的 JSON');
  assert.equal(errorText(400, 'plain bad request'), '请求参数有误：plain bad request');
  assert.equal(errorText(403, ''), '没有权限执行该操作（需要管理员）');
  assert.equal(errorText(404, ''), '资源不存在');
  assert.equal(errorText(500, 'db down'), '服务端错误（HTTP 500）：db down');
  assert.equal(errorText(418, ''), '请求失败（HTTP 418）');
});
