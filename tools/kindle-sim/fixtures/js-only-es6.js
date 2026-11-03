// 阳性对照：全部内容由 ES6 脚本渲染。
// KPW3 的 WebKit 会在解析期报错，整个脚本不执行 => 页面应判定为白屏。
const books = [
  { title: '示例书一', author: '甲' },
  { title: '示例书二', author: '乙' },
];

const render = () => {
  const root = document.getElementById('app');
  root.innerHTML = books.map((b) => `<div class="row">${b.title} — ${b.author}</div>`).join('');
};

render();
