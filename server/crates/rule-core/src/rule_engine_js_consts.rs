//! JS 规则使用的字符串资产（move-only 从 rule_engine_js.rs 拆出）。
//!
//! 内含内联 shim、繁简映射、MD5/T2S 等大段 JS 源码；与运行时逻辑分文件便于审阅。

pub(crate) const CN_TO_NUM_JS: &str = r#"
function cn2num(s){
  if(s==null) return "";
  s=String(s);
  if(/^[0-9]+$/.test(s)) return s;
  var d={'零':0,'一':1,'二':2,'两':2,'三':3,'四':4,'五':5,'六':6,'七':7,'八':8,'九':9};
  var u={'十':10,'百':100,'千':1000,'万':10000,'亿':100000000};
  var total=0,section=0,num=0;
  for(var i=0;i<s.length;i++){
    var ch=s[i];
    if(d[ch]!==undefined){ num=d[ch]; }
    else if(u[ch]!==undefined){
      if(ch==='万'||ch==='亿'){ if(num===0) num=1; section+=num*u[ch]; total+=section; section=0; num=0; }
      else { if(num===0) num=1; section+=num*u[ch]; num=0; }
    } else if(ch==='零'){ num=0; }
  }
  total+=section+num;
  return total===0 ? s : String(total);
}
"#;

/// UTF-8 编码为“字节字符串”，供 base64/hex/转义类函数使用（不依赖 escape/unescape）。
pub(crate) const JS_UTF8_HELPER: &str = r#"
function __utf8(s){
  s = encodeURIComponent(String(s));
  return s.replace(/%([0-9A-Fa-f]{2})/g, function(_, h){ return String.fromCharCode(parseInt(h, 16)); });
}
function __fromUtf8(bin){
  // 等价于 Java 的 new String(bytes, UTF_8)：非法字节用 U+FFFD 替换，绝不抛异常。
  // 之前用 decodeURIComponent 拼接 %xx，遇到脏字节会抛错并返回空串，
  // 导致 base64Decode/hexDecode 对“局部损坏但 ASCII 部分完好”的内容整段丢失。
  var out = "", i = 0, n = bin.length;
  while (i < n) {
    var c = bin.charCodeAt(i);
    if (c < 0x80) { out += String.fromCharCode(c); i += 1; continue; }
    var need, cp;
    if (c >= 0xC2 && c <= 0xDF) { need = 1; cp = c & 0x1F; }
    else if (c >= 0xE0 && c <= 0xEF) { need = 2; cp = c & 0x0F; }
    else if (c >= 0xF0 && c <= 0xF4) { need = 3; cp = c & 0x07; }
    else { out += "\uFFFD"; i += 1; continue; }
    if (i + need >= n) { out += "\uFFFD"; i += 1; continue; }
    var ok = true;
    for (var k = 1; k <= need; k++) {
      var cc = bin.charCodeAt(i + k);
      if ((cc & 0xC0) !== 0x80) { ok = false; break; }
      cp = (cp << 6) | (cc & 0x3F);
    }
    if (!ok) { out += "\uFFFD"; i += 1; continue; }
    if ((need === 1 && cp < 0x80) || (need === 2 && cp < 0x800) ||
        (need === 3 && cp < 0x10000) || cp > 0x10FFFF ||
        (cp >= 0xD800 && cp <= 0xDFFF)) {
      out += "\uFFFD"; i += 1; continue;
    }
    if (cp > 0xFFFF) {
      cp -= 0x10000;
      out += String.fromCharCode(0xD800 + (cp >> 10), 0xDC00 + (cp & 0x3FF));
    } else {
      out += String.fromCharCode(cp);
    }
    i += need + 1;
  }
  return out;
}
// 纯 JS base64，避免依赖宿主是否提供 btoa/atob（boa 0.19 默认不带）
var __B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function __b64enc(bin, urlSafe, pad){
  var alpha = urlSafe ? __B64URL : __B64;
  var out = "", i;
  for (i = 0; i < bin.length; i += 3) {
    var c1 = bin.charCodeAt(i);
    var c2 = i + 1 < bin.length ? bin.charCodeAt(i + 1) : 0;
    var c3 = i + 2 < bin.length ? bin.charCodeAt(i + 2) : 0;
    var e1 = c1 >> 2, e2 = ((c1 & 3) << 4) | (c2 >> 4);
    var e3 = ((c2 & 15) << 2) | (c3 >> 6), e4 = c3 & 63;
    out += alpha.charAt(e1) + alpha.charAt(e2);
    out += (i + 1 < bin.length ? alpha.charAt(e3) : "=");
    out += (i + 2 < bin.length ? alpha.charAt(e4) : "=");
  }
  if (pad === false) out = out.replace(/=+$/, "");
  return out;
}
function __b64dec(s){
  s = String(s).replace(/-/g, "+").replace(/_/g, "/").replace(/[^A-Za-z0-9+/=]/g, "");
  var bin = "", i;
  for (i = 0; i < s.length; i += 4) {
    var c1 = __B64.indexOf(s.charAt(i));
    var c2 = __B64.indexOf(s.charAt(i + 1));
    var c3 = s.charAt(i + 2) === "=" ? -1 : __B64.indexOf(s.charAt(i + 2));
    var c4 = s.charAt(i + 3) === "=" ? -1 : __B64.indexOf(s.charAt(i + 3));
    if (c1 < 0 || c2 < 0) continue;
    bin += String.fromCharCode((c1 << 2) | (c2 >> 4));
    if (c3 >= 0) bin += String.fromCharCode(((c2 & 15) << 4) | (c3 >> 2));
    if (c4 >= 0) bin += String.fromCharCode(((c3 & 3) << 6) | c4);
  }
  return bin;
}
// base64 变体（上游 JsExtensions.kt 签名）：
//   base64Encode(str) / base64Encode(str, flags)      flags 默认 2(NO_WRAP)，8=URL_SAFE，1=NO_PADDING
//   base64Decode(str) / base64Decode(str, charset) / base64Decode(str, flags)
//   base64DecodeToByteArray(str[, flags])
var __B64URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
function __b64encFlags(bin, flags){
  flags = (flags === undefined || flags === null) ? 2 : Number(flags);
  return __b64enc(bin, (flags & 8) !== 0, (flags & 1) === 0);
}
function __b64Bytes(s){ var bin = __b64dec(s), a = []; for (var i = 0; i < bin.length; i++) a.push(bin.charCodeAt(i)); return a; }
function __hexBin(h){
  h = String(h || "").replace(/[^0-9A-Fa-f]/g, "");
  if (h.length % 2 !== 0) return "";
  var out = "";
  // boa 0.19 没有实现 Annex B 的 String.prototype.substr，这里用 slice
  for (var i = 0; i < h.length; i += 2) out += String.fromCharCode(parseInt(h.slice(i, i + 2), 16));
  return out;
}
function __b64Charset(bin, charset){
  // 只实现上游规则源里常见的几种；GBK 等无 JS 解码表，退化为 UTF-8（与上游的 GBK 解码有差异）。
  var c = String(charset).toUpperCase().replace(/[_-]/g, "");
  if (c === "UTF8" || c === "") return __fromUtf8(bin);
  if (c === "LATIN1" || c === "ISO88591" || c === "ASCII" || c === "USASCII") return bin;
  if (c === "UTF16LE" || c === "UNICODELITTLE") {
    var out = "";
    for (var i = 0; i + 1 < bin.length; i += 2) out += String.fromCharCode(bin.charCodeAt(i) | (bin.charCodeAt(i + 1) << 8));
    return out;
  }
  return __fromUtf8(bin);
}
"#;

/// 纯 JS MD5（Joseph Myers 实现），避免依赖 boa 原生函数绑定。
/// 输入先经 __utf8 转成字节串，保证中文等非 ASCII 文本的摘要与 Java 侧一致。
pub(crate) const JS_MD5: &str = r#"
function __add32(a, b){ return (a + b) & 0xFFFFFFFF; }
function __cmn(q, a, b, x, s, t){
  a = __add32(__add32(a, q), __add32(x, t));
  return __add32((a << s) | (a >>> (32 - s)), b);
}
function __ff(a, b, c, d, x, s, t){ return __cmn((b & c) | ((~b) & d), a, b, x, s, t); }
function __gg(a, b, c, d, x, s, t){ return __cmn((b & d) | (c & (~d)), a, b, x, s, t); }
function __hh(a, b, c, d, x, s, t){ return __cmn(b ^ c ^ d, a, b, x, s, t); }
function __ii(a, b, c, d, x, s, t){ return __cmn(c ^ (b | (~d)), a, b, x, s, t); }
function __md5blk(s){
  var out = [], i;
  for (i = 0; i < 64; i += 4) {
    out[i >> 2] = s.charCodeAt(i) + (s.charCodeAt(i + 1) << 8) +
      (s.charCodeAt(i + 2) << 16) + (s.charCodeAt(i + 3) << 24);
  }
  return out;
}
function __md5cycle(x, k){
  var a = x[0], b = x[1], c = x[2], d = x[3];
  a = __ff(a,b,c,d,k[0],7,-680876936); d = __ff(d,a,b,c,k[1],12,-389564586);
  c = __ff(c,d,a,b,k[2],17,606105819); b = __ff(b,c,d,a,k[3],22,-1044525330);
  a = __ff(a,b,c,d,k[4],7,-176418897); d = __ff(d,a,b,c,k[5],12,1200080426);
  c = __ff(c,d,a,b,k[6],17,-1473231341); b = __ff(b,c,d,a,k[7],22,-45705983);
  a = __ff(a,b,c,d,k[8],7,1770035416); d = __ff(d,a,b,c,k[9],12,-1958414417);
  c = __ff(c,d,a,b,k[10],17,-42063); b = __ff(b,c,d,a,k[11],22,-1990404162);
  a = __ff(a,b,c,d,k[12],7,1804603682); d = __ff(d,a,b,c,k[13],12,-40341101);
  c = __ff(c,d,a,b,k[14],17,-1502002290); b = __ff(b,c,d,a,k[15],22,1236535329);
  a = __gg(a,b,c,d,k[1],5,-165796510); d = __gg(d,a,b,c,k[6],9,-1069501632);
  c = __gg(c,d,a,b,k[11],14,643717713); b = __gg(b,c,d,a,k[0],20,-373897302);
  a = __gg(a,b,c,d,k[5],5,-701558691); d = __gg(d,a,b,c,k[10],9,38016083);
  c = __gg(c,d,a,b,k[15],14,-660478335); b = __gg(b,c,d,a,k[4],20,-405537848);
  a = __gg(a,b,c,d,k[9],5,568446438); d = __gg(d,a,b,c,k[14],9,-1019803690);
  c = __gg(c,d,a,b,k[3],14,-187363961); b = __gg(b,c,d,a,k[8],20,1163531501);
  a = __gg(a,b,c,d,k[13],5,-1444681467); d = __gg(d,a,b,c,k[2],9,-51403784);
  c = __gg(c,d,a,b,k[7],14,1735328473); b = __gg(b,c,d,a,k[12],20,-1926607734);
  a = __hh(a,b,c,d,k[5],4,-378558); d = __hh(d,a,b,c,k[8],11,-2022574463);
  c = __hh(c,d,a,b,k[11],16,1839030562); b = __hh(b,c,d,a,k[14],23,-35309556);
  a = __hh(a,b,c,d,k[1],4,-1530992060); d = __hh(d,a,b,c,k[4],11,1272893353);
  c = __hh(c,d,a,b,k[7],16,-155497632); b = __hh(b,c,d,a,k[10],23,-1094730640);
  a = __hh(a,b,c,d,k[13],4,681279174); d = __hh(d,a,b,c,k[0],11,-358537222);
  c = __hh(c,d,a,b,k[3],16,-722521979); b = __hh(b,c,d,a,k[6],23,76029189);
  a = __hh(a,b,c,d,k[9],4,-640364487); d = __hh(d,a,b,c,k[12],11,-421815835);
  c = __hh(c,d,a,b,k[15],16,530742520); b = __hh(b,c,d,a,k[2],23,-995338651);
  a = __ii(a,b,c,d,k[0],6,-198630844); d = __ii(d,a,b,c,k[7],10,1126891415);
  c = __ii(c,d,a,b,k[14],15,-1416354905); b = __ii(b,c,d,a,k[5],21,-57434055);
  a = __ii(a,b,c,d,k[12],6,1700485571); d = __ii(d,a,b,c,k[3],10,-1894986606);
  c = __ii(c,d,a,b,k[10],15,-1051523); b = __ii(b,c,d,a,k[1],21,-2054922799);
  a = __ii(a,b,c,d,k[8],6,1873313359); d = __ii(d,a,b,c,k[15],10,-30611744);
  c = __ii(c,d,a,b,k[6],15,-1560198380); b = __ii(b,c,d,a,k[13],21,1309151649);
  a = __ii(a,b,c,d,k[4],6,-145523070); d = __ii(d,a,b,c,k[11],10,-1120210379);
  c = __ii(c,d,a,b,k[2],15,718787259); b = __ii(b,c,d,a,k[9],21,-343485551);
  x[0] = __add32(a, x[0]); x[1] = __add32(b, x[1]);
  x[2] = __add32(c, x[2]); x[3] = __add32(d, x[3]);
}
function __rhex(n){
  var s = "", j;
  for (j = 0; j < 4; j++) {
    s += ("0" + ((n >> (j * 8)) & 255).toString(16)).slice(-2);
  }
  return s;
}
function __md5(s){
  s = __utf8(s);
  var state = [1732584193, -271733879, -1732584194, 271733878], i;
  for (i = 64; i <= s.length; i += 64) { __md5cycle(state, __md5blk(s.substring(i - 64, i))); }
  s = s.substring(i - 64);
  var tail = [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0];
  for (i = 0; i < s.length; i++) { tail[i >> 2] |= s.charCodeAt(i) << ((i % 4) << 3); }
  tail[i >> 2] |= 0x80 << ((i % 4) << 3);
  if (i > 55) { __md5cycle(state, tail); for (i = 0; i < 16; i++) tail[i] = 0; }
  tail[14] = s.length * 8;
  __md5cycle(state, tail);
  var out = "";
  for (i = 0; i < 4; i++) { out += __rhex(state[i]); }
  return out;
}
"#;

/// 给 boa 注入的 java / book 垫片。
/// 覆盖可离线实现的高频内置；依赖网络（ajax/post）、Cookie/WebView 的桥接函数显式抛错，
/// DOM 三件套（getElements/getString/getStringList）在 Phase 1 走 Rust 预抽取查表，
/// 只有字面量选择器可用，动态拼串仍显式抛出具名错误。
/// 简→繁常用字表（供 `java.t2s`）。只覆盖常见字；未收录字原样返回。
pub(crate) const JS_T2S: &str = r#"var __T2S = {"爱":"愛","罢":"罷","摆":"擺","败":"敗","办":"辦","帮":"幫","宝":"寶","报":"報","备":"備","贝":"貝","笔":"筆","毕":"畢","边":"邊","变":"變","标":"標","表":"錶","别":"彆","宾":"賓","补":"補","参":"參","惨":"慘","灿":"燦","仓":"倉","产":"產","长":"長","尝":"嘗","车":"車","彻":"徹","尘":"塵","陈":"陳","衬":"襯","称":"稱","惩":"懲","迟":"遲","冲":"衝","丑":"醜","础":"礎","处":"處","触":"觸","传":"傳","疮":"瘡","闯":"闖","创":"創","纯":"純","词":"詞","从":"從","丛":"叢","窜":"竄","达":"達","带":"帶","贷":"貸","担":"擔","单":"單","胆":"膽","当":"當","党":"黨","荡":"蕩","档":"檔","导":"導","敌":"敵","邓":"鄧","递":"遞","点":"點","电":"電","垫":"墊","钓":"釣","调":"調","叠":"疊","东":"東","动":"動","冻":"凍","斗":"鬥","独":"獨","读":"讀","断":"斷","对":"對","队":"隊","吨":"噸","夺":"奪","堕":"墮","儿":"兒","尔":"爾","发":"發","罚":"罰","阀":"閥","飞":"飛","费":"費","废":"廢","纷":"紛","坟":"墳","奋":"奮","粪":"糞","风":"風","丰":"豐","冯":"馮","凤":"鳳","肤":"膚","妇":"婦","复":"復","该":"該","干":"乾","赶":"趕","冈":"岡","刚":"剛","岗":"崗","纲":"綱","钢":"鋼","个":"個","给":"給","巩":"鞏","沟":"溝","构":"構","购":"購","谷":"穀","顾":"顧","刮":"颳","关":"關","观":"觀","馆":"館","惯":"慣","广":"廣","归":"歸","龟":"龜","规":"規","国":"國","过":"過","汉":"漢","号":"號","轰":"轟","后":"後","胡":"鬍","壶":"壺","沪":"滬","护":"護","划":"劃","怀":"懷","坏":"壞","欢":"歡","环":"環","还":"還","回":"迴","汇":"匯","会":"會","秽":"穢","荤":"葷","浑":"渾","伙":"夥","获":"獲","击":"擊","鸡":"雞","积":"積","极":"極","际":"際","继":"繼","家":"傢","价":"價","艰":"艱","歼":"殲","舰":"艦","姜":"薑","浆":"漿","桨":"槳","奖":"獎","讲":"講","酱":"醬","胶":"膠","浇":"澆","骄":"驕","娇":"嬌","搅":"攪","缴":"繳","觉":"覺","较":"較","阶":"階","节":"節","洁":"潔","结":"結","借":"藉","仅":"僅","惊":"驚","竞":"競","旧":"舊","剧":"劇","据":"據","惧":"懼","卷":"捲","决":"決","绝":"絕","军":"軍","开":"開","克":"剋","壳":"殼","夸":"誇","矿":"礦","亏":"虧","困":"睏","腊":"臘","蜡":"蠟","来":"來","赖":"賴","兰":"蘭","拦":"攔","栏":"欄","烂":"爛","垒":"壘","类":"類","里":"裡","礼":"禮","丽":"麗","厉":"厲","励":"勵","离":"離","历":"歷","帘":"簾","联":"聯","怜":"憐","炼":"煉","练":"練","粮":"糧","两":"兩","辆":"輛","疗":"療","辽":"遼","了":"瞭","猎":"獵","临":"臨","邻":"鄰","岭":"嶺","庐":"廬","芦":"蘆","炉":"爐","陆":"陸","虏":"虜","录":"錄","虑":"慮","乱":"亂","罗":"羅","萝":"蘿","络":"絡","妈":"媽","马":"馬","骂":"罵","买":"買","麦":"麥","卖":"賣","迈":"邁","脉":"脈","猫":"貓","么":"麼","门":"門","闷":"悶","梦":"夢","弥":"彌","觅":"覓","绵":"綿","面":"麵","庙":"廟","灭":"滅","亩":"畝","恼":"惱","脑":"腦","拟":"擬","酿":"釀","鸟":"鳥","聂":"聶","宁":"寧","农":"農","齐":"齊","岂":"豈","启":"啟","气":"氣","弃":"棄","牵":"牽","签":"簽","迁":"遷","窍":"竅","窃":"竊","亲":"親","轻":"輕","庆":"慶","琼":"瓊","穷":"窮","秋":"鞦","区":"區","驱":"驅","趋":"趨","权":"權","劝":"勸","确":"確","让":"讓","扰":"擾","热":"熱","认":"認","荣":"榮","绒":"絨","洒":"灑","伞":"傘","丧":"喪","扫":"掃","涩":"澀","杀":"殺","晒":"曬","纱":"紗","设":"設","绅":"紳","审":"審","声":"聲","胜":"勝","圣":"聖","师":"師","湿":"濕","时":"時","实":"實","适":"適","势":"勢","兽":"獸","书":"書","术":"術","树":"樹","帅":"帥","双":"雙","谁":"誰","顺":"順","说":"說","丝":"絲","松":"鬆","苏":"蘇","诉":"訴","虽":"雖","随":"隨","台":"臺","态":"態","摊":"攤","滩":"灘","坛":"壇","叹":"嘆","汤":"湯","烫":"燙","涛":"濤","体":"體","条":"條","铁":"鐵","听":"聽","厅":"廳","头":"頭","图":"圖","涂":"塗","团":"團","洼":"窪","袜":"襪","弯":"彎","万":"萬","网":"網","韦":"韋","违":"違","围":"圍","为":"為","卫":"衛","温":"溫","闻":"聞","问":"問","稳":"穩","务":"務","雾":"霧","牺":"犧","习":"習","系":"係","戏":"戲","细":"細","虾":"蝦","吓":"嚇","鲜":"鮮","纤":"纖","咸":"鹹","显":"顯","险":"險","现":"現","献":"獻","乡":"鄉","详":"詳","响":"響","向":"嚮","协":"協","挟":"挾","泻":"瀉","谢":"謝","兴":"興","须":"須","许":"許","悬":"懸","选":"選","学":"學","寻":"尋","压":"壓","亚":"亞","严":"嚴","盐":"鹽","阳":"陽","养":"養","样":"樣","痒":"癢","谣":"謠","药":"藥","爷":"爺","业":"業","叶":"葉","页":"頁","义":"義","议":"議","亿":"億","忆":"憶","艺":"藝","隐":"隱","应":"應","营":"營","拥":"擁","优":"優","忧":"憂","邮":"郵","余":"餘","鱼":"魚","与":"與","屿":"嶼","语":"語","吁":"籲","郁":"鬱","誉":"譽","渊":"淵","园":"園","员":"員","圆":"圓","缘":"緣","远":"遠","愿":"願","跃":"躍","云":"雲","杂":"雜","灾":"災","赞":"讚","赃":"贓","脏":"臟","凿":"鑿","枣":"棗","灶":"竈","责":"責","择":"擇","则":"則","泽":"澤","贼":"賊","赠":"贈","扎":"紮","张":"張","帐":"帳","战":"戰","这":"這","针":"針","征":"徵","争":"爭","挣":"掙","症":"癥","证":"證","只":"隻","帜":"幟","质":"質","种":"種","众":"眾","昼":"晝","烛":"燭","筑":"築","庄":"莊","装":"裝","妆":"妝","壮":"壯","状":"狀","准":"準","浊":"濁","总":"總","钻":"鑽","纵":"縱","邹":"鄒","诅":"詛","组":"組","话":"話","记":"記","写":"寫","诗":"詩","请":"請","难":"難","乐":"樂","进":"進","岁":"歲","间":"間","见":"見","龙":"龍","颜":"顏","恋":"戀","泪":"淚","华":"華","画":"畫"};"#;

/// `cache` 全局（由引擎注入缓存管理对象）。
/// 上游 CacheManager 的方法名是 `get / put / delete`（另有 getFromMemory / putMemory /
/// deleteMemory，CacheManager.kt:60-160），**没有** set/remove/clear。
/// 本实现是每作用域一份的字符串 map：`get` 未命中返回 null（同上游返回 String?），
/// put 的对象/数组按 JSON 串存储（上游存 Any）。
pub(crate) const JS_CACHE: &str = r#"
var __cache = {};
var cache = {
  get: function(k){
    k = String(k);
    return Object.prototype.hasOwnProperty.call(__cache, k) ? __cache[k] : null;
  },
  getFromMemory: function(k){ return cache.get(k); },
  put: function(k, v){
    k = String(k);
    if (typeof v === "string") { __cache[k] = v; return; }
    try { __cache[k] = JSON.stringify(v); } catch (e) { __cache[k] = String(v); }
  },
  putMemory: function(k, v){ cache.put(k, v); },
  delete: function(k){ delete __cache[String(k)]; },
  deleteMemory: function(k){ cache.delete(k); },
  // 上游规则 `CacheManager.getFile/putFile`：key 只当不透明标识（服务端把它 md5 成文件名），
  // 不允许用它直接拼路径；落盘目录固定为部署侧 `<data_dir>/cache/rule`。
  getFile: function(k){ return __nativeCacheFileGet(k == null ? "" : String(k)); },
  putFile: function(k, v, saveTime){
    return __nativeCacheFilePut(
      k == null ? "" : String(k),
      String(v),
      saveTime == null ? 0 : Math.trunc(Number(saveTime))
    );
  }
};
"#;

/// JS 执行结果里回传 cache 变更的分隔符（结果串 + 分隔符 + cache JSON）。
pub(crate) const JS_CACHE_SENTINEL: &str = "\u{1}F1CACHE\u{1}";

/// 结果串尾部的“规则自身耗时”（毫秒）：预算只该算规则代码，不该算固定垫片/lib 的解析。
pub(crate) const JS_RULE_MS_SENTINEL: &str = "\u{1}F1RULEMS\u{1}";

/// 结果串尾部的 `java.refreshTocUrl()` 标记（`1` 表示本次 JS 请求刷新目录地址）。
pub(crate) const JS_REFRESH_TOC_SENTINEL: &str = "\u{1}F1TOC\u{1}";
/// 结果串尾部回传的 `book.bookUrl/book.tocUrl` 变更（`pre_update_js` 可能改写）。
pub(crate) const JS_BOOK_SENTINEL: &str = "\u{1}F1BOOK\u{1}";
/// 结果串尾部回传的 JS 变量表（`java.put`/`book.putVariable` 等写入）。
pub(crate) const JS_VARS_SENTINEL: &str = "\u{1}F1VARS\u{1}";

pub(crate) const JAVA_BOOK_SHIM: &str = r#"
var __vars = {};
// DOM 桥 Phase 1 的预抽取结果（由 Rust 在 eval 前按字面量选择器算好并赋值）
var __DOM = {};
var __DOMS = {};
// java.setContent 设置的“当前文档”。null 表示未设置，仍走原始页面的预抽取表；
// 设置后 java.getElements/getString/getStringList 才按需解析这个文档（惰性求值）。
var __content = null;
var book = {
  bookUrl: __bookUrl,
  tocUrl: __bookTocUrl,
  canUpdate: true,
  name: __bookName,
  author: __bookAuthor,
  kind: __bookKind,
  intro: __bookIntro,
  coverUrl: __bookCoverUrl,
  getVariable: function(k){ return __vars[k] !== undefined ? __vars[k] : ""; },
  putVariable: function(k,v){ __vars[k] = String(v); }
};
// 书源 key（= bookSourceUrl）由 Rust 侧在每次求值前注入。`get/put` 对齐上游规则
// `BaseSource.get/put`：按书源分桶的字符串变量（CacheManager key `v_<sourceKey>_<key>`）。
// 本服务为进程内存储，不落盘；`put` 返回写入的字符串值，`get` 缺省返回空串。
var source = {
  key: __sourceKey,
  loginUrl: __sourceLoginUrl,
  getKey: function(){ return __sourceKey; },
  get: function(k){ return __nativeSourceVarGet(__sourceKey, k == null ? "" : String(k)); },
  put: function(k, v){
    var s = String(v);
    return __nativeSourceVarPut(__sourceKey, k == null ? "" : String(k), s);
  }
};
// 上游规则 `cookie` 全局：落到 Rust 侧按书源分桶的进程内 Cookie 存储（req.rs COOKIE_JAR），
// 不新建全局 map；get 无值返回空串，set/remove/clear 返回空串（可直接拼进 {{ }} 模板）。
var cookie = {
  getCookie: function(url){ return __nativeCookieGet(__sourceKey, url == null ? "" : String(url)); },
  setCookie: function(url, value){
    __nativeCookieSet(__sourceKey, url == null ? "" : String(url), value == null ? "" : String(value));
    return "";
  },
  removeCookie: function(url){
    __nativeCookieRemove(__sourceKey, url == null ? "" : String(url));
    return "";
  },
  clear: function(){ __nativeCookieClear(__sourceKey); return ""; }
};
// 上游规则 `java.connect` / `java.startBrowserAwait` 返回 StrResponse。服务端没有 WebView/人工验证，
// 这里用与 java.ajax 相同的原生抓取层发请求，并返回同形状的响应对象（body/url/code/header）。
// 抓取失败按上游规则的容错方式把错误写进 body（调用方 `.body().includes(...)` 不会抛异常）。
function __httpResponse(url, header){
  var u = String(url == null ? "" : url);
  var h = header == null ? "" : (typeof header === "string" ? header : JSON.stringify(header));
  var body = "", code = 200;
  try {
    body = __nativeAjax(u, "GET", "", h);
  } catch (e) {
    body = "connect(" + u + ") error\n" + String(e && e.message ? e.message : e);
    code = 0;
  }
  return {
    body: function(){ return body; },
    getBody: function(){ return body; },
    url: function(){ return u; },
    getUrl: function(){ return u; },
    code: function(){ return code; },
    getCode: function(){ return code; },
    header: function(){ return ""; },
    headers: function(){ return ""; },
    toString: function(){ return body; }
  };
}
var java = {
  toNumChapter: function(s){ return s==null ? "" : cn2num(String(s)); },
  md5Encode: function(s){ return __md5(s == null ? "" : String(s)); },
  md5Encode16: function(s){ return __md5(s == null ? "" : String(s)).substring(8, 24); },
  md5: function(s){ return java.md5Encode(s); },
  // 简繁转换：常用字表（书源多用于书名/作者检索的繁简互通）。未收录的生僻字原样保留。
  t2s: function(s){
    s = String(s == null ? "" : s);
    var out = "";
    for (var i = 0; i < s.length; i++) { out += (__T2S[s.charAt(i)] || s.charAt(i)); }
    return out;
  },
  base64Encode: function(s, flags){ try { return __b64encFlags(__utf8(s), flags); } catch(e){ return ""; } },
  base64Decode: function(s, arg){
    try {
      var bin = __b64dec(s);
      if (typeof arg === "string") return __b64Charset(bin, arg);
      return __fromUtf8(bin);
    } catch(e){ return ""; }
  },
  base64DecodeToString: function(s){ return java.base64Decode(s); },
  base64DecodeToByteArray: function(s, flags){ try { return __b64Bytes(s); } catch(e){ return []; } },
  encodeURI: function(s){ return encodeURIComponent(String(s)); },
  decodeURI: function(s){ try { return decodeURIComponent(String(s)); } catch(e){ return ""; } },
  encodeURIComponent: function(s){ return encodeURIComponent(String(s)); },
  decodeURIComponent: function(s){ try { return decodeURIComponent(String(s)); } catch(e){ return ""; } },
  strToInt: function(s){ var n = parseInt(String(s), 10); return isNaN(n) ? 0 : n; },
  intToStr: function(n){ return String(n); },
  hexEncode: function(s){
    var b = __utf8(s), out = "";
    for (var i = 0; i < b.length; i++) { out += ("0" + b.charCodeAt(i).toString(16)).slice(-2); }
    return out;
  },
  hexEncodeToString: function(s){ return java.hexEncode(s); },
  hexDecode: function(s){ try { return __fromUtf8(__hexBin(s)); } catch(e){ return ""; } },
  hexDecodeToString: function(s){ return java.hexDecode(s); },
  hexDecodeToByteArray: function(s){
    try {
      var bin = __hexBin(s), a = [];
      for (var i = 0; i < bin.length; i++) a.push(bin.charCodeAt(i));
      return a;
    } catch(e){ return []; }
  },
  random: function(max){
    if (max === undefined || max === null) return Math.random();
    return Math.floor(Math.random() * Number(max));
  },
  randomInt: function(min, max){
    min = Number(min || 0); max = Number(max);
    return min + Math.floor(Math.random() * (max - min));
  },
  timeFormat: function(t, f){
    if (typeof t === "string" && f === undefined) { f = t; t = undefined; }
    if (t === undefined || t === null || t === "") t = Date.now();
    var n = Number(t), d;
    if (!isNaN(n)) {
      // 上游规则的 time 多为毫秒；小于 1e11 视为秒级时间戳
      d = new Date(n < 100000000000 ? n * 1000 : n);
    } else { d = new Date(String(t)); }
    if (isNaN(d.getTime())) return "";
    var p = function(x, l){ x = String(x); while (x.length < (l || 2)) x = "0" + x; return x; };
    var map = {
      yyyy: p(d.getFullYear(), 4), MM: p(d.getMonth() + 1), dd: p(d.getDate()),
      HH: p(d.getHours()), mm: p(d.getMinutes()), ss: p(d.getSeconds()),
      SSS: p(d.getMilliseconds(), 3)
    };
    f = f || "yyyy-MM-dd HH:mm:ss";
    return String(f).replace(/yyyy|MM|dd|HH|mm|ss|SSS/g, function(k){ return map[k]; });
  },
  htmlFormat: function(s){
    return String(s == null ? "" : s).replace(/&/g, "&amp;").replace(/</g, "&lt;")
      .replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  },
  htmlFormat2: function(s){
    var t = String(s == null ? "" : s).replace(/<br\s*\/?>/gi, "\n");
    t = t.replace(/<[^>]+>/g, "");
    return t.replace(/&nbsp;/g, " ").replace(/&lt;/g, "<").replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"').replace(/&#39;/g, "'").replace(/&amp;/g, "&");
  },
  escape: function(s){ return escape(String(s)); },
  unescape: function(s){ return unescape(String(s)); },
  put: function(k, v){ book.putVariable(k, v); },
  get: function(k){ return book.getVariable(k); },
  getVariable: function(k){ return book.getVariable(k); },
  putVariable: function(k, v){ book.putVariable(k, v); },
  deleteVariable: function(k){ delete __vars[k]; },
  log: function(){},
  toast: function(){},
  longToast: function(){},
  // 上游规则: java.ajax(url[, method[, body[, headers]]]) -> 响应文本。
  // 由 Rust 原生 __nativeAjax 同步联网，复用 req 的 15s 超时 / 5MiB / 私网策略。
  ajax: function(url, method, body, headers){
    return __nativeAjax(String(url == null ? "" : url),
      method == null ? "GET" : String(method),
      body == null ? "" : String(body),
      headers == null ? "" : (typeof headers === "string" ? headers : JSON.stringify(headers)));
  },
  post: function(url, body, headers){ return java.ajax(url, "POST", body, headers); },
  // 上游规则 `java.importScript(path)`：下载脚本文本供规则 `eval(String(...))`。本服务只接受
  // http(s)（进程内 TTL 缓存，见 __nativeImportScript）；file/content/storage 形态会读取
  // 部署机本地文件，等于把书源变成任意文件读取面，故显式报未支持而不是静默失败。
  importScript: function(path){
    return String(__nativeImportScript(path == null ? "" : String(path)));
  },
  // 上游规则 `java.connect(url[, header])`：返回 StrResponse（body/url/code/header）。
  connect: function(url, header){ return __httpResponse(url, header); },
  // 上游规则 `java.head(url[, header])`：HEAD 请求，返回带响应头的 StrResponse（不读正文）。
  // 书源 jsLib（如哔哩漫画 Reload）常用 `.header('etag')` 拿缓存指纹。
  head: function(url, header){
    var u = String(url == null ? "" : url);
    var h = header == null ? "" : (typeof header === "string" ? header : JSON.stringify(header));
    var r = JSON.parse(__nativeHead(u, h));
    var hs = r.headers || {};
    return {
      body: function(){ return ""; },
      getBody: function(){ return ""; },
      url: function(){ return r.url || u; },
      getUrl: function(){ return r.url || u; },
      code: function(){ return r.status || 0; },
      getCode: function(){ return r.status || 0; },
      header: function(n){
        var k = String(n == null ? "" : n).toLowerCase();
        for (var x in hs) { if (x.toLowerCase() === k) return String(hs[x]); }
        return "";
      },
      headers: function(){ return hs; },
      toString: function(){ return ""; }
    };
  },
  // DOM 桥：`java.setContent(html)` 之后按需在“当前文档”上求值（Rust 原生解析，
  // 不再依赖预抽取的只读表）；未 setContent 时仍用 Phase 1 的字面量选择器快路。
  setContent: function(html){ __content = (html == null) ? "" : String(html); },
  getElements: function(rule, index){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      var arr = JSON.parse(__nativeSelect(__content, key, true));
      if (typeof index === "number") {
        return (index >= 0 && index < arr.length) ? arr[index] : "";
      }
      return arr;
    }
    if (rule == null || !__DOM.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getElements（仅支持字面量选择器参数）");
    }
    var arr = __DOM[key].slice(0);
    if (typeof index === "number") {
      return (index >= 0 && index < arr.length) ? arr[index] : "";
    }
    return arr;
  },
  getStringList: function(rule){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      return JSON.parse(__nativeSelect(__content, key, true));
    }
    if (rule == null || !__DOM.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getStringList（仅支持字面量选择器参数）");
    }
    return __DOM[key].slice(0);
  },
  getString: function(rule){
    var key = rule == null ? "" : String(rule);
    if (__content !== null) {
      return JSON.parse(__nativeSelect(__content, key, false));
    }
    if (rule == null || !__DOMS.hasOwnProperty(key)) {
      throw new Error("UNSUPPORTED_JS_API:java.getString（仅支持字面量选择器参数）");
    }
    return __DOMS[key];
  },
  // 上游规则的 java.getElement：从“当前页面”取第一个匹配元素（jsoup Element）。
  getElement: function(rule){
    var doc = new JsoupDocument(__jsoupParse(__page == null ? "" : String(__page), __base));
    return doc.selectFirst(rule == null ? "" : String(rule));
  },
  // 上游规则 java.aesBase64DecodeToString(data, key, transformation, iv)
  aesBase64DecodeToString: function(data, key, transformation, iv){
    return __nativeAesDecodeToString(
      data == null ? "" : String(data),
      key == null ? "" : String(key),
      transformation == null ? "" : String(transformation),
      iv == null ? "" : String(iv));
  },
  // 上游规则 java.createSymmetricCrypto(transformation, key, iv) -> {encrypt, decrypt}
  createSymmetricCrypto: function(transformation, key, iv){
    var tf = transformation == null ? "" : String(transformation);
    var k = __keyArg(key);
    var v = __keyArg(iv);
    return {
      encrypt: function(data){
        return JSON.parse(__nativeSymmetric("encrypt", tf, k, v, JSON.stringify(__toByteArray(data))));
      },
      decrypt: function(data){
        return JSON.parse(__nativeSymmetric("decrypt", tf, k, v, JSON.stringify(__toByteArray(data))));
      }
    };
  },
  // 上游规则 java.bytesToStr(bytes) -> UTF-8 字符串
  bytesToStr: function(bytes){ return __nativeBytesToStr(JSON.stringify(__toByteArray(bytes))); },
  // 上游规则 java.timeFormatUTC(time, format, offsetHours)
  timeFormatUTC: function(t, f, offset){
    if (typeof t === "string" && f === undefined) { f = t; t = undefined; }
    if (t === undefined || t === null || t === "") t = Date.now();
    var n = Number(t), d;
    if (!isNaN(n)) d = new Date(n < 100000000000 ? n * 1000 : n);
    else d = new Date(String(t));
    if (isNaN(d.getTime())) return "";
    if (offset !== undefined && offset !== null && offset !== "") {
      d = new Date(d.getTime() + Number(offset) * 3600 * 1000);
    }
    var p = function(x, l){ x = String(x); while (x.length < (l || 2)) x = "0" + x; return x; };
    var map = {
      yyyy: p(d.getUTCFullYear(), 4), MM: p(d.getUTCMonth() + 1), dd: p(d.getUTCDate()),
      HH: p(d.getUTCHours()), mm: p(d.getUTCMinutes()), ss: p(d.getUTCSeconds()),
      SSS: p(d.getUTCMilliseconds(), 3)
    };
    f = f || "yyyy-MM-dd HH:mm:ss";
    return String(f).replace(/yyyy|MM|dd|HH|mm|ss|SSS/g, function(k){ return map[k]; });
  },
  // 上游规则 java.refreshTocUrl：标记“需要用 bookUrl 重新取目录”，由 toc 流程消费。
  refreshTocUrl: function(){ __refreshToc = true; },
  getCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.getCookie（本服务无 Cookie 桥接）"); },
  setCookie: function(){ throw new Error("UNSUPPORTED_JS_API:java.setCookie（本服务无 Cookie 桥接）"); },
  // 上游规则 `java.startBrowserAwait(url, title[, refetchAfterSuccess])`：内置浏览器人工过验证后返回页面。
  // 服务端没有人机交互界面，这里直接抓取目标页并返回 StrResponse；遇到站点质询页时 body 就是质询页，
  // 由书源规则自行判断，行为与“验证失败但拿到了响应”一致。
  startBrowserAwait: function(url){ return __httpResponse(url, null); },
  // 上游规则 `java.startBrowser(url, title)`：只打开内置浏览器，无返回值。服务端无 UI，等价空操作。
  startBrowser: function(){ return ""; },
  // 上游规则 `java.webView(html, url, js)`：无 WebView 时只覆盖“取整页源码”的形态——
  // html 非空直接返回 html；否则抓取 url；传入 js 需要页面内 DOM 执行，显式报未支持。
  webView: function(html, url, js){
    if (js != null && String(js).trim() !== "") {
      throw new Error("UNSUPPORTED_JS_API:java.webView(js)（本服务无 WebView，无法执行页面内脚本）");
    }
    if (url != null && String(url).trim() !== "") {
      try { return __nativeAjax(String(url), "GET", "", ""); } catch(e) { return ""; }
    }
    return html == null ? "" : String(html);
  },
  // 这两个 API 依赖 WebView 观察最终资源地址/跳转地址，抓取层拿不到等价信号，保持显式未支持。
  webViewGetSource: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetSource（本服务无 WebView）"); },
  webViewGetOverrideUrl: function(){ throw new Error("UNSUPPORTED_JS_API:java.webViewGetOverrideUrl（本服务无 WebView）"); },
};

// ===== org.jsoup（按 147 源实际调用面实现）=====
// 后端是 scraper(html5ever)。jsoup 专有扩展伪类（:contains/:matches/:eq 等）由 Rust 侧显式抛错。
function JsoupElement(h){
  // `__h` 不可枚举：`for (i in element)` 不应看到内部句柄
  Object.defineProperty(this, "__h", { value: h, enumerable: false });
}
JsoupElement.prototype.attr = function(k){ return String(__jsoupAttr(this.__h, String(k))); };
JsoupElement.prototype.text = function(){ return String(__jsoupText(this.__h)); };
JsoupElement.prototype.html = function(){ return String(__jsoupHtml(this.__h)); };
JsoupElement.prototype.outerHtml = function(){ return String(__jsoupOuterHtml(this.__h)); };
JsoupElement.prototype.toString = function(){ return this.outerHtml(); };
JsoupElement.prototype.hasAttr = function(k){ return !!__jsoupHasAttr(this.__h, String(k)); };
JsoupElement.prototype.is = function(sel){ return !!__jsoupIs(this.__h, String(sel)); };
JsoupElement.prototype.tagName = function(){ return String(__jsoupTag(this.__h)); };
JsoupElement.prototype.parent = function(){
  var x = __jsoupParent(this.__h);
  return (x === null || x === undefined) ? null : new JsoupElement(x);
};
JsoupElement.prototype.nextSiblingElements = function(){
  return new JsoupElements(JSON.parse(__jsoupNextSiblingElements(this.__h)));
};
JsoupElement.prototype.select = function(sel){
  return new JsoupElements(JSON.parse(__jsoupSelect(this.__h, String(sel))));
};
JsoupElement.prototype.selectFirst = function(sel){
  return this.select(sel).first();
};
function JsoupElements(list){
  // `__list`/`length` 必须不可枚举：书源里大量 `for (i in a)` 依赖只枚举数字下标
  // （Rhino 对 Java List 的 for-in 行为），多一个属性就会把 `a['__list']` 当元素用。
  var arr = list || [];
  Object.defineProperty(this, "__list", { value: arr, enumerable: false });
  for (var i = 0; i < arr.length; i++) this[i] = new JsoupElement(arr[i]);
  Object.defineProperty(this, "length", { value: arr.length, enumerable: false });
}
JsoupElements.prototype.size = function(){ return this.length; };
JsoupElements.prototype.get = function(i){
  i = Number(i) || 0;
  return (i >= 0 && i < this.length) ? this[i] : null;
};
JsoupElements.prototype.first = function(){ return this.length ? this[0] : null; };
JsoupElements.prototype.last = function(){ return this.length ? this[this.length - 1] : null; };
// jsoup `Elements.eq(i)` 返回只含一个元素的 Elements（不是 Element）。
JsoupElements.prototype.eq = function(i){
  var n = Number(i) || 0;
  if (n < 0) n = this.length + n;
  return new JsoupElements((n >= 0 && n < this.length) ? [this.__list[n]] : []);
};
JsoupElements.prototype.attr = function(k){ return this.length ? this[0].attr(k) : ""; };
// jsoup `Elements.text()` 是**所有元素文本拼接**（空格分隔），`html()` 是所有内层 HTML 拼接。
JsoupElements.prototype.text = function(){
  var out = [];
  for (var i = 0; i < this.length; i++) out.push(this[i].text());
  return out.join(" ");
};
JsoupElements.prototype.html = function(){
  var out = [];
  for (var i = 0; i < this.length; i++) out.push(this[i].html());
  return out.join("");
};
JsoupElements.prototype.outerHtml = function(){ return this.length ? this[0].outerHtml() : ""; };
JsoupElements.prototype.hasAttr = function(k){ return this.length ? this[0].hasAttr(k) : false; };
JsoupElements.prototype.is = function(sel){
  for (var i = 0; i < this.length; i++) { if (this[i].is(sel)) return true; }
  return false;
};
JsoupElements.prototype.toArray = function(){
  var a = [];
  for (var i = 0; i < this.length; i++) a.push(this[i]);
  return a;
};
JsoupElements.prototype.select = function(sel){
  var all = [];
  for (var i = 0; i < this.length; i++) {
    var h = JSON.parse(__jsoupSelect(this.__list[i], String(sel)));
    for (var j = 0; j < h.length; j++) all.push(h[j]);
  }
  return new JsoupElements(all);
};
function JsoupDocument(h){
  Object.defineProperty(this, "__h", { value: h, enumerable: false });
}
JsoupDocument.prototype.select = function(sel){
  return new JsoupElements(JSON.parse(__jsoupSelect(this.__h, String(sel))));
};
JsoupDocument.prototype.selectFirst = function(sel){ return this.select(sel).first(); };
JsoupDocument.prototype.text = function(){
  var b = this.select("body").first();
  return b ? b.text() : "";
};
JsoupDocument.prototype.html = function(){
  var b = this.select("body").first();
  return b ? b.html() : "";
};
JsoupDocument.prototype.outerHtml = function(){ return String(__jsoupOuterHtml(this.__h)); };
// 原型方法默认 enumerable，会让 `for (i in elements)` 枚举到方法名（Boa/Rhino 的
// for-in 含继承的可枚举属性）。书源里大量 for-in 只应看到数字下标，这里统一隐藏。
function __jsHideProto(proto){
  for (var k in proto) {
    if (Object.prototype.hasOwnProperty.call(proto, k)) {
      Object.defineProperty(proto, k, {
        value: proto[k], enumerable: false, writable: true, configurable: true
      });
    }
  }
}
__jsHideProto(JsoupElement.prototype);
__jsHideProto(JsoupElements.prototype);
__jsHideProto(JsoupDocument.prototype);
var org = { jsoup: { Jsoup: {
  parse: function(html, base){
    var s = (html == null) ? "" : (typeof html === "string" ? html
      : (typeof html.outerHtml === "function" ? html.outerHtml() : String(html)));
    return new JsoupDocument(__jsoupParse(s, base == null ? __base : String(base)));
  },
  isValid: function(html){
    try { org.jsoup.Jsoup.parse(html); return true; } catch (e) { return false; }
  }
}}};

// ===== 字节/Buffer 辅助（哔哩漫画 image_decode 链路）=====
function __toByteArray(d){
  if (d == null) return [];
  if (typeof d === "string") {
    var b = __utf8(d), a = [];
    for (var i = 0; i < b.length; i++) a.push(b.charCodeAt(i));
    return a;
  }
  if (typeof d.length === "number") {
    var a = [];
    for (var i = 0; i < d.length; i++) a.push(Number(d[i]) & 255);
    return a;
  }
  return [];
}
// key/iv 参数：字节数组按 JSON 传给原生；字符串交给 Rust 按 UTF-8（上游规则的
// String.toByteArray()）转换，避免 `String([1,2,3])` 变成 "1,2,3"。
function __keyArg(x){
  if (x == null) return "";
  if (typeof x === "string") return x;
  return JSON.stringify(__toByteArray(x));
}
var Buffer = {
  concat: function(list){
    var out = [];
    if (!list) return out;
    for (var i = 0; i < list.length; i++) {
      var b = list[i];
      if (b == null) continue;
      var a = __toByteArray(b);
      for (var j = 0; j < a.length; j++) out.push(a[j]);
    }
    return out;
  }
};
"#;
