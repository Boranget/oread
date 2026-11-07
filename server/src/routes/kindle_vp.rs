//! /k 的 vp 部分（move-only 从 kindle.rs 拆出）。

use super::*;

/// 请求级视口（CSS px）。几何全部由它推导，不再写死 1000/1268；
/// 优先级：请求参数 `vw`/`vh` > 环境变量默认（见 `Config.kindle_vw/vh`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Vp {
    pub(super) w: usize,
    pub(super) h: usize,
    /// 请求是否显式带了 `vw`。显式时 viewport 锁 `width={w}`（真机 1:1 不缩放）；
    /// 缺参时用 `width=device-width`，让补参脚本能量出真实 CSS 宽度。
    pub(super) fixed: bool,
}

impl Vp {
    /// 缺参/越界时回落到默认视口，保证任何尺寸都不会算出负高度。
    pub(super) fn resolve(vw: Option<i64>, vh: Option<i64>, d: (usize, usize)) -> Self {
        let ok = |v: Option<i64>| v.filter(|n| (480..=2400).contains(n)).map(|n| n as usize);
        Self {
            w: ok(vw).unwrap_or(d.0),
            h: ok(vh).unwrap_or(d.1),
            fixed: ok(vw).is_some(),
        }
    }
    /// 版心宽 = 视口宽 − 左右留白 ×2（1072 → 1000）
    pub(super) fn page_w(self) -> usize {
        self.w.saturating_sub(SIDE_PAD * 2)
    }
    /// 漫画图宽 = 版心宽 − 左右留白 ×2（1072 → 928，即切片版心宽 dw）
    pub(super) fn content_w(self) -> usize {
        self.w.saturating_sub(SIDE_PAD * 4)
    }
    /// 文档高 = 请求带回来的运行时实测可见高（`K_VP_JS` 现算，见 `layout_h` 上方说明）。
    /// 不再做任何固定 px 收缩/夹取：顶栏、底栏、正文切片都从这一个值派生。
    pub(super) fn layout_h(self) -> usize {
        self.h
    }
    /// 阅读页底栏总高（含底部安全区）：随运行时可用高派生，约 5.2%（1231 → 64），
    /// 不写死像素；顶栏、底栏、正文区都从 `layout_h()` 这一个值派生。
    pub(super) fn bar_h(self) -> usize {
        (self.layout_h() * 13 / 250).max(48)
    }
    /// 底栏内容高（去掉 `SAFE_BOTTOM` 安全区）
    pub(super) fn bar_content_h(self) -> usize {
        self.bar_h().saturating_sub(SAFE_BOTTOM).max(40)
    }
    /// 阅读页正文容器高 = 文档高 − 底栏
    pub(super) fn body_h(self) -> usize {
        self.layout_h().saturating_sub(self.bar_h())
    }
    /// 带顶栏页面的正文区高 = 文档高 − 顶栏 − 1（1268 → 1107）
    pub(super) fn nav_h(self) -> usize {
        self.layout_h().saturating_sub(NAV_H + 1)
    }
    /// 小说正文可用高 = 正文容器 − 顶栏 − 顶部留白 − 页码行（1268 → 968）
    pub(super) fn text_h(self) -> usize {
        self.body_h()
            .saturating_sub(READ_NAV_H + READ_PAD_TOP + READ_PAGENO_H)
    }
    /// 漫画整图/单片可用高 = 文档高 − 底栏 − 8（同一运行时高派生）
    pub(super) fn content_h(self) -> usize {
        self.layout_h().saturating_sub(self.bar_h() + 8)
    }
    /// 左右翻页热区宽 = 视口宽 / 3（1072 → 357，CSS 用 33% 自适应）
    #[allow(dead_code)] // 仅单测引用：渲染改用 CSS 百分比，不再需要服务端算像素
    pub(super) fn hot_w(self) -> usize {
        self.w / 3
    }
    /// 书架每页本数与行高：优先保持 10 本/页，把可用高（`nav_h − 底栏`）等分给 10 行，
    /// 行高上限 112；视口极矮（行高不足 56）才减少每页本数，保证列表永不超过 stage。
    pub(super) fn shelf_layout(self) -> (usize, usize) {
        let avail = self.nav_h().saturating_sub(BAR_H);
        let row_h = (avail / K_SHELF_PAGE).clamp(56, 112);
        let per_page = if avail / K_SHELF_PAGE >= 56 {
            K_SHELF_PAGE
        } else {
            (avail / 56).clamp(1, K_SHELF_PAGE)
        };
        (per_page, row_h)
    }
}

impl Default for Vp {
    fn default() -> Self {
        Self {
            w: K_W,
            h: K_H,
            fixed: true,
        }
    }
}

/// 阅读页每页行数预算 = 可用高 ÷ 行高（字号四档 28/32/38/20 对应 46/52/62/36）。
/// 动态算出，不再有 24/21/17/25 这类硬编码常量。
pub(super) fn line_budget(vp: Vp, line: usize) -> usize {
    (vp.text_h() / line.max(1)).max(1)
}
/// 段间距（行）：WeRead 实测段距 0.6 行（默认档 27px）
/// 实际排版用 `para_gap_px`（整数 px）；本常量保留为换算基准，仅单测引用。
#[allow(dead_code)]
pub(super) const PARA_GAP_LINES: f64 = 0.6;
/// 段间距像素（向下取整，保证渲染高度不超过行预算）
pub(super) const fn para_gap_px(line: usize) -> usize {
    (line * 6) / 10
}
/// 目录每页章节数 = 每列行数 × 2；每列行数按可用高实算（72px/行，上限 15 行/列）。
pub(super) fn toc_page_size(vp: Vp) -> usize {
    ((vp.nav_h().saturating_sub(BAR_H)) / 72).clamp(1, K_TOC_PAGE / K_TOC_COLS) * K_TOC_COLS
}
/// 搜索结果每页条数 = （正文区 − 表单 88 − 翻页条 64）/ 112；有回退说明/升级按钮时再少一条。
pub(super) fn search_page_size(vp: Vp, extra_row: bool) -> usize {
    let base = ((vp.nav_h().saturating_sub(BAR_H + 88)) / 112).clamp(1, K_SEARCH_PAGE);
    if extra_row {
        base.saturating_sub(1).max(1)
    } else {
        base
    }
}
/// 目录每页章节数上限：两列各 15 行 × 72px = 1080（实际行数按可用高实算）
pub(super) const K_TOC_PAGE: usize = 30;
/// 目录每列行数
pub(super) const K_TOC_COLS: usize = 2;
/// 书架每页本数上限：10 行 × 112px = 1120（实际行高按可用高等分，见 `Vp::shelf_layout`）
pub(super) const K_SHELF_PAGE: usize = 10;
/// 搜索结果每页条数上限：9 行 × 112px = 1008（实际条数按可用高实算）
pub(super) const K_SEARCH_PAGE: usize = 9;
/// /k 渲染前对正文纯文本的硬截断（防御超大章节把分页临时索引撑爆内存）
pub(super) const K_MAX_TEXT_CHARS: usize = 800_000;
/// 超长章节分块：每块最多 20 万字符，按块分页，避免一次性按 80 万字构建全量页表
pub(super) const K_CHUNK_CHARS: usize = 200_000;
/// 字号 Cookie（与 `?fs=` 二选一，显式参数优先）
pub(super) const K_FS_COOKIE: &str = "k_fs";

/// 运行时视口高脚本（ES5，注入 `<head>`，首帧前生效；不落 Cookie、不写 sessionStorage）。
///
/// 每次进页面现算可用高，再把 `vh=<算出值>` 带回 URL：
///
///   1. `outerHeight − 1`（真机 1232 → 1231，最接近真实可用高）；
///   2. `screen.availHeight` 做上限保护；
///   3. 两者都取不到才退 `innerHeight`（1268 只作机型识别与兜底）。
///
/// 首次进入（链接没带 `vh`）用默认值渲染一次，脚本在 `<head>` 里立刻 `location.replace`；
/// `load` 后把同一 `vh` 补到页内所有链接，翻页不再二次重载。同机型反复进入结果一致。
pub(super) const K_VP_JS: &str = "(function(){try{\
var d=document.documentElement,ih=window.innerHeight||0,sh=window.screen||{};\
function mn(a,b){if(!(b>0))return a;if(!(a>0))return b;return a<b?a:b}\
var H=(window.outerHeight||0)-1,AH=(sh.availHeight||0);\
if(!(H>200))H=AH;\
if(AH>200)H=mn(H,AH);\
if(!(H>200))H=ih;\
if(!(H>200))H=(d&&d.clientHeight>0)?d.clientHeight:ih;\
var s=location.search.replace(/^\\?/,''),p=s?s.split('&'):[],o=[],cur=null;\
for(var i=0;i<p.length;i++){var k=p[i].split('=')[0];\
if(k=='vh')cur=p[i].substring(3);if(p[i]&&k!='vh')o.push(p[i]);}\
window.__kvh=H;\
if(cur!=''+H){o.push('vh='+H);location.replace(location.pathname+'?'+o.join('&'));return}\
function fix(){if(!document.links)return;try{\
for(var j=0;j<document.links.length;j++){var a=document.links[j],h=a.getAttribute('href');\
if(!h||h.charAt(0)=='#'||h.indexOf('javascript:')==0||h.indexOf('vh=')>-1)continue;\
a.setAttribute('href',h+(h.indexOf('?')>-1?'&':'?')+'vh='+H)}}catch(e){}}\
if(document.addEventListener){document.addEventListener('load',fix,false)}setTimeout(fix,0)\
}catch(E){try{window.__kerr&&window.__kerr((E&&E.message)||E,0,'vp')}catch(x){}}})()";

/// 全局错误兜底（ES5，注入 `<head>`）。错误统一走 `window.__kerr`，用 `<img>` 打点上报
/// （`stage=gerr`），不依赖 `load` 与 XHR。**不再拦截 touchmove、也不再上报 touch 事件**
/// （真机实测 touch 不触发；且已明确不做任何「骗浏览器」的补偿）。
pub(super) const K_GUARD_JS: &str = "(function(){\
function e(m,l,st){try{var i=new Image();\
i.src='/api/k/probe?stage='+st+'&err='+encodeURIComponent((''+(m||'-')).substring(0,60))+\
'&ln='+(l||0)+'&vw_param='+encodeURIComponent((''+(location.search||'')).substring(0,80))}catch(x){}}\
function ke(m,s,l){e(m,l,'gerr');return false}\
window.__kerr=e;window.onerror=ke})()";

/// 自报探针（ES5，零依赖，不重载页面）：**只保留 `<img>` 单通道**（v0.3.19 的教训：
/// img 请求 204 触发 `onerror` 后又走 XHR = 每个 stage 打两条，统计翻倍）。
/// 打点时机：`sync`（`setTimeout(...,0)` 延后到首次布局完成，避免 `inner_h=0`）、
/// `load`、`scroll`（200ms 节流 + 800ms settle）、`click`。整段包在 `try/catch` 里，
/// 异常走 `window.__kerr`（`stage=probe`）。
/// v0.3.22 瘦身：删掉标定/vmh 字段（`calib`/`meas_y`/`vv_off`/`vw_meta_height`/`meta_h`/
/// `visible_h`），改为上报运行时参与计算的原值与结果：`outer_h`/`inner_h`/`screen_h`/
/// `avail_h` + `rt_h`（`window.__kvh` 现算高）+ `lines`（`#k-stage` 的 `data-lb` 每页行数）。
/// 溢出证据看 `doc_h` / `documentElement.scrollHeight` / `outer_minus_doc` /
/// `outer_minus_scroll`；`body_scroll_h` 在该引擎里是假象，只作对拍。
/// `click` 上报 `pageX/pageY`（真机 touch 不触发，click 可用）与内层滚动容器 `scrollTop`。
pub(super) const K_PROBE_JS: &str = "(function(){try{\
function p(n){var m=new RegExp('[?&]'+n+'=([^&]*)').exec(location.search);return m?m[1]:''}\
function v(x){return typeof x=='number'?x:'-'}\
function bar(){return document.getElementById('k-bar')}\
function btn(){var b=bar();if(!b)return -1;try{return Math.round(b.getBoundingClientRect().top)}catch(e){\
return typeof b.offsetTop=='number'?b.offsetTop:-1}}\
function bt(){var n=btn();return n<0?'-':n}\
function bb(){var b=bar();if(!b)return '-';try{return Math.round(b.getBoundingClientRect().bottom)}catch(e){return b.offsetTop+b.offsetHeight}}\
function bre(){var b=document.body;if(!b)return '-';try{return Math.round(b.getBoundingClientRect().bottom)}catch(e){return '-'}}\
function lb(){var b=document.body;if(!b||!b.children)return '-';var m=-1;try{for(var i=0;i<b.children.length;i++){\
var r=b.children[i].getBoundingClientRect(),x=r.top+(r.height||(r.bottom-r.top));if(x>m)m=x}}catch(e){return '-'}return m>=0?Math.round(m):'-'}\
function stb(){try{return (document.body&&document.body.scrollTop)||0}catch(e){return 0}}\
function std(){try{var d=document.documentElement;return (d&&d.scrollTop)||0}catch(e){return 0}}\
function sy(){try{return Math.round(window.scrollY||window.pageYOffset||0)}catch(e){return 0}}\
var max_seen=0,base_bt=-1;\
function py(){var y=stb();if(!y)y=std();if(!y)y=sy();\
y=Math.round(y);if(y>max_seen)max_seen=y;return y}\
function btd(){if(base_bt<0)return '-';var n=btn();if(n<0)return '-';var d=n-base_bt;\
if(d<0)d=-d;if(d>max_seen)max_seen=d;return n-base_bt}\
function lbn(){try{var s=document.getElementById('k-stage');\
var n=s?parseInt(s.getAttribute('data-lb'),10):NaN;return isNaN(n)?'-':n}catch(e){return '-'}}\
function ist(){try{var s=document.getElementById('k-scroll');\
return s?Math.round(s.scrollTop||0):'-'}catch(e){return '-'}}\
function qs(ev){var d=document.documentElement,dpr=window.devicePixelRatio||1,sn=window.screen||{},b=document.body,\
st=document.getElementById('k-stage'),sb=stb(),sd=std(),dlt=btd(),\
dh=parseFloat(d.style.height);if(!(dh>0))dh=d.clientHeight;var oh=window.outerHeight||0;\
return 'vw_param='+p('vw')+'&vh_param='+p('vh')+'&inner_w='+innerWidth+'&inner_h='+innerHeight+\
'&client_w='+d.clientWidth+'&client_h='+d.clientHeight+'&scroll_w='+d.scrollWidth+'&scroll_h='+d.scrollHeight+\
'&dpr='+dpr+'&fs='+p('fs')+'&path='+encodeURIComponent(location.pathname)+\
'&screen_w='+v(sn.width)+'&screen_h='+v(sn.height)+'&avail_h='+v(sn.availHeight)+\
'&outer_w='+v(window.outerWidth)+'&outer_h='+v(window.outerHeight)+\
'&doc_h='+Math.round(dh)+'&doc_minus_outer='+Math.round(dh-oh)+'&doc_minus_client='+Math.round(dh-d.clientHeight)+\
'&outer_minus_doc='+Math.round(oh-dh)+'&rt_h='+v(window.__kvh)+'&lines='+lbn()+\
'&body_h='+v(b&&b.clientHeight)+'&stage_h='+v(st&&st.clientHeight)+\
'&bar_top='+bt()+'&bar_bottom='+bb()+'&bar_rect_top='+bt()+'&bar_top_delta='+dlt+\
'&page_y='+py()+'&doc_top='+sd+'&scroll_top_body='+sb+'&scroll_top_doc='+sd+'&scroll_y='+sy()+\
'&max_seen='+max_seen+\
'&body_scroll_h='+v(b&&b.scrollHeight)+\
'&body_rect_bottom='+bre()+'&lowest_el_bottom='+lb()+'&scroll_evt='+(ev?1:0)+\
'&outer_minus_scroll='+Math.round(oh-d.scrollHeight)+'&inner_scroll_top='+ist()}\
function s(stage,src){try{var u='/api/k/probe?'+qs(src=='scroll'?1:0)+\
'&stage='+stage+'&move_src='+(src||'-');var i=new Image();i.src=u}catch(e){\
try{window.__kerr&&window.__kerr((e&&e.message)||e,0,'probe')}catch(x){}}}\
var last=0,stt=0;\
function mv(src){var t=new Date().getTime();if(t-last<200)return;last=t;s('move',src)}\
function ld(){if(base_bt<0)base_bt=btn();s('load','-')}\
function rs(){mv('scroll');if(stt)clearTimeout(stt);\
stt=setTimeout(function(){s('move','settle')},800)}\
function cl(ev){try{var d=document.documentElement,x=ev.pageX,y=ev.pageY;\
if(typeof x!='number')x=ev.clientX;if(typeof y!='number')y=ev.clientY;\
var i=new Image();i.src='/api/k/probe?stage=click&click_x='+Math.round(x)+'&click_y='+Math.round(y)+\
'&scroll_h='+d.scrollHeight+'&outer_minus_scroll='+Math.round((window.outerHeight||0)-d.scrollHeight)+\
'&inner_scroll_top='+ist()+'&vw_param='+encodeURIComponent((''+(location.search||'')).substring(0,60))}catch(e){}}\
setTimeout(function(){s('sync','-')},0);\
if(window.addEventListener){window.addEventListener('load',ld,false);\
window.addEventListener('scroll',rs,false);document.addEventListener('click',cl,false)}\
else if(window.attachEvent){window.attachEvent('onload',ld);window.attachEvent('onscroll',rs)}\
}catch(E){try{window.__kerr&&window.__kerr((E&&E.message)||E,0,'probe')}catch(x){}}})()";

/// 四档字号。每页行数由 `line_budget(vp, line)` 按视口高动态算出。
pub(super) struct FsSpec {
    pub(super) size: usize,
    pub(super) line: usize,
    pub(super) label: &'static str,
}

pub(super) const FS_SPECS: [FsSpec; 4] = [
    // 927/28 ≈ 33 字/行；行数 = 正文可用高 ÷ 46。
    FsSpec {
        size: 28,
        line: 46,
        label: "小",
    },
    // 字号口径：真机实测 32px/52px，保留 32px。此处显式写死 32，且全局
    // `-webkit-text-size-adjust:none` 已关闭老引擎的字号自动放大——实测 32/52，勿低于 30。
    FsSpec {
        size: 32,
        line: 52,
        label: "中",
    },
    FsSpec {
        size: 38,
        line: 62,
        label: "大",
    },
    FsSpec {
        size: 20,
        line: 36,
        label: "超小",
    },
];
