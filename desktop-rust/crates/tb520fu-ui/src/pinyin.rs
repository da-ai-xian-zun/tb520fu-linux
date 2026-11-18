// SPDX-License-Identifier: MIT
use std::collections::HashMap;
use std::sync::OnceLock;

struct PinyinTables {
    syl_chars: HashMap<&'static str, &'static str>,
    words: HashMap<&'static str, &'static [&'static str]>,
    char_to_py: HashMap<char, &'static str>,
}

static TABLES: OnceLock<PinyinTables> = OnceLock::new();

fn tables() -> &'static PinyinTables {
    TABLES.get_or_init(|| {
        let mut syl_chars = HashMap::with_capacity(PY_SYL.len());
        let mut char_to_py = HashMap::with_capacity(2800);
        for &(syl, chars) in PY_SYL {
            syl_chars.insert(syl, chars);
            for ch in chars.chars() {
                char_to_py.entry(ch).or_insert(syl);
            }
        }
        let mut words = HashMap::with_capacity(PY_WORDS.len());
        for &(k, arr) in PY_WORDS {
            words.insert(k, arr);
        }
        PinyinTables {
            syl_chars,
            words,
            char_to_py,
        }
    })
}

/// Greedy longest-match Mandarin syllable segmentation (`"nihaoshijie"` -> `("ni'hao'shi'jie", ["ni","hao","shi","jie"])`).
pub fn segment_pinyin(raw: &str) -> (String, Vec<String>) {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if clean.is_empty() {
        return (String::new(), Vec::new());
    }
    let tb = tables();
    let bytes = clean.as_bytes();
    let mut i = 0usize;
    let mut segs: Vec<String> = Vec::new();
    while i < bytes.len() {
        let max_len = (bytes.len() - i).min(6);
        let mut matched: Option<&str> = None;
        for len in (1..=max_len).rev() {
            let sub = &clean[i..i + len];
            if tb.syl_chars.contains_key(sub) {
                matched = Some(sub);
                break;
            }
        }
        if let Some(m) = matched {
            segs.push(m.to_string());
            i += m.len();
        } else {
            segs.push(clean[i..i + 1].to_string());
            i += 1;
        }
    }
    let preedit = segs.join("'");
    (preedit, segs)
}

/// Offline Pinyin candidate lookup (exact phrase -> prefix phrase -> multi-syllable char assembly -> single syllable chars).
pub fn lookup_pinyin_candidates(raw: &str) -> (String, Vec<String>) {
    let clean: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if clean.is_empty() {
        return (String::new(), Vec::new());
    }
    let tb = tables();
    let (preedit, segs) = segment_pinyin(&clean);
    let mut out: Vec<String> = Vec::new();
    fn push_unique(out: &mut Vec<String>, s: String) {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }

    // 1. Exact phrase match
    if let Some(arr) = tb.words.get(clean.as_str()) {
        for &w in *arr {
            push_unique(&mut out, w.to_string());
        }
    }

    // 2. Assembled top character per segmented syllable (e.g. "nihaoshijie" -> "你好时接")
    if segs.len() >= 2 {
        let mut assembled = String::new();
        let mut all_ok = true;
        for seg in &segs {
            if let Some(chars) = tb.syl_chars.get(seg.as_str()) {
                if let Some(first) = chars.chars().next() {
                    assembled.push(first);
                    continue;
                }
            }
            all_ok = false;
            break;
        }
        if all_ok {
            push_unique(&mut out, assembled);
        }
    }

    // 3. Prefix phrase matches for short partial input
    if clean.len() >= 2 && out.len() < 10 {
        let mut pref_hits: Vec<(&str, &&[&str])> = tb
            .words
            .iter()
            .filter(|(k, _)| k.starts_with(clean.as_str()) && **k != clean.as_str())
            .map(|(k, v)| (*k, v))
            .collect();
        pref_hits.sort_by_key(|(k, _)| k.len());
        for (_, arr) in pref_hits.into_iter().take(6) {
            for &w in *arr {
                push_unique(&mut out, w.to_string());
            }
        }
    }

    // 4. First syllable's individual characters (frequency-ordered)
    if let Some(first_seg) = segs.first() {
        if let Some(chars) = tb.syl_chars.get(first_seg.as_str()) {
            for ch in chars.chars().take(18) {
                push_unique(&mut out, ch.to_string());
            }
        } else {
            // Single initial consonant fallback (e.g. "z" -> top chars of "zhi", "za", "zi", "zuo")
            let mut syls: Vec<(&str, &str)> = tb
                .syl_chars
                .iter()
                .filter(|(k, _)| k.starts_with(first_seg.as_str()))
                .map(|(k, v)| (*k, *v))
                .collect();
            syls.sort_by_key(|(k, _)| k.len());
            for (_, chars) in syls.into_iter().take(6) {
                for ch in chars.chars().take(3) {
                    push_unique(&mut out, ch.to_string());
                }
            }
        }
    }

    if out.is_empty() {
        out.push(clean);
    }
    (preedit, out)
}

/// Convert mixed Chinese/ASCII text into `(pinyin_initials, full_pinyin)` for instant App Menu search!
pub fn pinyin_keys_for_text(text: &str) -> (String, String) {
    let tb = tables();
    let mut initials = String::new();
    let mut full = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            let lc = ch.to_ascii_lowercase();
            initials.push(lc);
            full.push(lc);
        } else if let Some(py) = tb.char_to_py.get(&ch) {
            if let Some(first) = py.chars().next() {
                initials.push(first);
            }
            full.push_str(py);
        }
    }
    (initials, full)
}

/// Match `query` against app `name` or `app_id` using case-insensitive substring, Pinyin initials, or full Pinyin!
pub fn matches_app_query(name: &str, app_id: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    if name.to_lowercase().contains(&q) || app_id.to_lowercase().contains(&q) {
        return true;
    }
    let q_alpha: String = q.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if q_alpha.is_empty() {
        return false;
    }
    let (init, full) = pinyin_keys_for_text(name);
    init.contains(&q_alpha) || full.contains(&q_alpha)
}

const PY_SYL: &[(&str, &str)] = &[
    ("a", "啊阿呵吖锕腌"), ("ai", "爱哎矮唉艾碍挨癌哀埃蔼隘"), ("an", "安按暗岸案俺鞍氨庵谙"), ("ang", "昂肮盎"), ("ao", "奥熬傲澳凹袄嗷懊翱遨鳌"),
    ("e", "饿额呃鹅俄恶厄鄂娥扼讹"), ("ei", "诶欸"), ("en", "嗯恩摁"), ("eng", "鞥"), ("er", "二而儿耳尔饵贰洱迩"), ("o", "哦噢喔"), ("ou", "欧偶呕藕鸥殴怄沤"),
    ("ba", "吧把八爸拔巴霸罢坝扒疤芭"), ("bai", "白百摆败拜柏佰掰稗"), ("ban", "办半班般板版搬伴扮瓣斑颁"), ("bang", "帮棒绑邦榜膀傍磅谤蚌"),
    ("bao", "包报保宝抱饱暴爆薄胞堡豹"), ("bei", "被北备背倍杯悲贝辈碑卑狈"), ("ben", "本笨奔苯畚贲"), ("beng", "崩蹦绷甭迸泵"),
    ("bi", "比必笔毕闭避币彼鼻逼壁臂"), ("bian", "边变便遍编辩鞭扁辨贬"), ("biao", "表标彪膘裱飙镖"), ("bie", "别憋瘪鳖"),
    ("bin", "宾彬滨濒鬓斌缤"), ("bing", "并病冰兵饼丙柄秉禀"), ("bo", "波播博拨伯薄剥玻驳脖勃泊"), ("bu", "不部步布补捕卜簿埠哺"),
    ("pa", "怕爬啪趴帕琶葩"), ("pai", "排派拍牌徘湃"), ("pan", "判盘盼攀叛畔潘磐"), ("pang", "旁胖庞乓膀耪"), ("pao", "跑泡炮抛袍刨咆"),
    ("pei", "配陪赔培佩胚沛裴"), ("pen", "喷盆湓"), ("peng", "朋碰捧棚蓬膨烹鹏澎篷"), ("pi", "皮批披匹疲脾劈屁辟僻啤坯"),
    ("pian", "片篇骗偏便翩"), ("piao", "票飘漂瓢嫖瞟"), ("pie", "撇瞥苤"), ("pin", "品拼贫频聘拚"), ("ping", "平评瓶凭屏苹萍坪乒"),
    ("po", "破迫坡婆颇泼魄泊"), ("pou", "剖掊"), ("pu", "普铺扑仆朴谱葡浦蒲瀑曝"),
    ("ma", "吗妈马嘛骂麻码蚂玛抹"), ("mai", "买卖埋麦迈脉"), ("man", "满慢漫曼瞒馒蔓蛮"), ("mang", "忙盲茫莽芒氓"),
    ("mao", "毛帽冒猫貌矛茂贸卯锚"), ("me", "么麽"), ("mei", "没每美妹煤眉媒霉梅枚魅"), ("men", "们门闷扪焖懑"),
    ("meng", "梦猛蒙盟孟萌朦锰"), ("mi", "米密迷秘蜜眯弥谜觅泌"), ("mian", "面免棉眠绵勉冕缅"), ("miao", "秒妙苗描庙瞄渺喵"),
    ("mie", "灭蔑咩篾"), ("min", "民敏闽皿悯抿"), ("ming", "明名命鸣铭冥茗"), ("miu", "谬缪"), ("mo", "末摸模莫默磨魔墨膜陌摩抹"),
    ("mou", "某谋眸牟"), ("mu", "目木母幕慕模牧墓暮穆姆亩"),
    ("fa", "发法罚乏伐阀"), ("fan", "反饭翻犯烦范凡番繁返泛帆"), ("fang", "方放房防仿访芳妨纺"), ("fei", "非飞费肥废肺匪菲啡沸"),
    ("fen", "分份粉纷奋愤坟芬氛焚"), ("feng", "风封丰峰疯锋逢奉缝凤蜂讽"), ("fo", "佛"), ("fou", "否缶"), ("fu", "服父复府福副负富夫付附扶"),
    ("da", "大打达答搭哒"), ("dai", "带代待袋戴呆贷逮怠"), ("dan", "但单担蛋淡胆弹旦丹诞"), ("dang", "当党档挡荡铛"),
    ("dao", "到道倒导刀岛盗稻蹈悼"), ("de", "的得地德"), ("dei", "得"), ("den", "扽"), ("deng", "等灯登邓凳瞪蹬"),
    ("di", "地第底低弟敌帝递滴迪抵笛"), ("dia", "嗲"), ("dian", "点电店典垫殿淀滇颠"), ("diao", "掉调钓吊雕刁碉"),
    ("die", "爹跌叠碟蝶迭谍"), ("ding", "定顶订丁钉盯鼎"), ("diu", "丢铥"), ("dong", "动东懂冬洞冻董栋"), ("dou", "都斗豆抖逗兜陡"),
    ("du", "度读都独毒堵渡杜肚督赌镀"), ("duan", "段短断端锻缎"), ("dui", "对队堆兑怼"), ("dun", "顿吨蹲盾敦炖钝"), ("duo", "多夺躲朵度堕舵剁惰"),
    ("ta", "他她它塔踏塌"), ("tai", "太台态抬胎泰汰"), ("tan", "谈探弹叹坦摊贪滩坛毯炭"), ("tang", "躺堂糖汤趟唐塘烫倘"),
    ("tao", "套讨逃桃淘陶涛掏"), ("te", "特忑忒慝"), ("tei", "忒"), ("teng", "疼腾藤誊"), ("ti", "题提体替踢梯啼剔蹄"),
    ("tian", "天田甜填添舔"), ("tiao", "条跳挑调眺迢"), ("tie", "铁贴帖餮"), ("ting", "听停庭挺厅亭艇"), ("tong", "同通痛统铜童桶筒"),
    ("tou", "头投透偷骰"), ("tu", "图土突途徒涂吐兔"), ("tuan", "团湍抟"), ("tui", "推退腿褪颓蜕"), ("tun", "吞屯臀囤豚"), ("tuo", "托拖脱妥拓驼"),
    ("na", "那拿哪呐纳娜"), ("nai", "奶乃耐奈氖"), ("nan", "南难男楠喃腩"), ("nang", "囊馕囔"), ("nao", "脑闹恼挠瑙淖"),
    ("ne", "呢讷哪"), ("nei", "内哪馁"), ("nen", "嫩恁"), ("neng", "能"), ("ni", "你呢泥尼逆拟腻妮霓"), ("nian", "年念粘碾捻"),
    ("niang", "娘酿"), ("niao", "鸟尿袅"), ("nie", "捏聂孽涅镍蹑"), ("nin", "您"), ("ning", "宁凝拧柠泞狞"), ("niu", "牛扭纽钮妞拗"),
    ("nong", "弄农浓脓侬"), ("nou", "耨"), ("nu", "怒努奴弩驽"), ("nuan", "暖"), ("nun", "黁"), ("nuo", "诺挪懦糯娜"), ("nv", "女钕"), ("nve", "虐疟"),
    ("la", "啦拉辣蜡腊喇"), ("lai", "来赖莱睐濑籁"), ("lan", "蓝兰烂懒览栏拦篮滥澜"), ("lang", "浪狼郎朗廊琅"),
    ("lao", "老劳牢捞姥唠涝烙"), ("le", "了乐勒"), ("lei", "累类泪雷垒擂"), ("leng", "冷愣棱楞"), ("li", "里力理立利李离例历礼丽粒"),
    ("lia", "俩"), ("lian", "连脸练联恋炼莲廉帘怜"), ("liang", "两量亮凉良粮辆梁"), ("liao", "了聊料疗辽撩廖"),
    ("lie", "列烈裂劣猎咧"), ("lin", "林临邻淋琳磷"), ("ling", "另领零令灵铃龄凌岭"), ("liu", "六流留刘柳溜硫浏馏榴"), ("lo", "咯"),
    ("long", "龙笼隆聋拢垄"), ("lou", "楼漏搂露陋娄"), ("lu", "路录陆露鲁炉鹿芦卤"), ("luan", "乱卵峦孪挛鸾"),
    ("lun", "论轮伦沦抡纶"), ("luo", "落罗络洛逻萝骆裸锣"), ("lv", "绿率旅律虑驴铝履吕屡"), ("lve", "略掠"),
    ("ga", "嘎尬噶咖旮"), ("gai", "该改盖概钙丐"), ("gan", "干感敢赶甘肝杆"), ("gang", "刚港钢岗纲缸"), ("gao", "高告搞稿糕膏"),
    ("ge", "个哥各歌格隔割革阁搁"), ("gei", "给"), ("gen", "跟根亘艮"), ("geng", "更耕庚耿梗羹"), ("gong", "工公共功攻宫供恭贡"),
    ("gou", "够狗构购沟钩勾苟"), ("gu", "故古股鼓顾骨谷姑孤固估菇"), ("gua", "挂刮瓜寡卦褂"), ("guai", "怪乖拐"),
    ("guan", "关管观官馆惯冠灌贯"), ("guang", "光广逛咣胱"), ("gui", "规贵鬼归跪柜轨桂"), ("gun", "滚棍衮"), ("guo", "过国果锅裹郭"),
    ("ka", "卡咖喀咔"), ("kai", "开凯慨楷揩铠"), ("kan", "看砍刊堪坎侃"), ("kang", "抗康扛炕慷亢"), ("kao", "考靠烤拷犒"),
    ("ke", "可课客科克刻渴颗壳棵咳"), ("kei", "剋"), ("ken", "肯啃恳垦"), ("keng", "坑吭铿"), ("kong", "空控恐孔"), ("kou", "口扣抠寇叩"),
    ("ku", "苦哭酷库裤枯窟"), ("kua", "夸跨垮挎胯"), ("kuai", "快块筷会侩脍"), ("kuan", "宽款髋"), ("kuang", "况狂矿框旷筐"),
    ("kui", "亏愧溃馈葵魁窥"), ("kun", "困昆捆坤"), ("kuo", "扩括阔廓"),
    ("ha", "哈蛤"), ("hai", "还孩海害嗨亥骇"), ("han", "喊汉含寒汗韩旱憾函"), ("hang", "行航杭夯巷"), ("hao", "好号毫豪耗浩郝"),
    ("he", "和合何河喝盒核贺荷赫"), ("hei", "黑嘿"), ("hen", "很恨狠痕"), ("heng", "横恒哼衡亨"), ("hong", "红洪轰哄宏虹烘鸿"),
    ("hou", "后候厚猴吼喉侯"), ("hu", "户呼护湖胡忽虎互糊壶狐乎"), ("hua", "话花化画华滑划哗"), ("huai", "坏怀淮槐徊踝"),
    ("huan", "还换欢环缓患幻唤"), ("huang", "黄慌荒皇谎晃煌"), ("hui", "会回灰挥汇恢辉毁惠悔绘"), ("hun", "婚混魂昏浑荤"), ("huo", "或活火获货伙祸惑霍"),
    ("ji", "几机级记极及即基积技际急计辑"), ("jia", "家加价假架甲佳嘉夹驾"), ("jian", "见间件建简减检尖坚监健渐剪键"),
    ("jiang", "将讲江奖降僵蒋酱"), ("jiao", "叫教交较角脚觉焦骄胶郊"), ("jie", "接解界节结姐街借介届阶洁截"),
    ("jin", "进今近金紧仅尽斤禁劲"), ("jing", "经精京静境竟镜景警惊净井"), ("jiong", "窘炯迥"), ("jiu", "就九旧久酒救究纠"),
    ("ju", "局举句具聚据剧居巨拒菊"), ("juan", "卷捐倦娟眷绢"), ("jue", "觉决绝角掘倔诀爵"), ("jun", "军均君菌俊峻"),
    ("qi", "起其气期七汽器奇企骑齐旗"), ("qia", "恰洽掐卡"), ("qian", "前钱千签浅欠迁铅牵潜谦"), ("qiang", "强墙枪抢腔呛"),
    ("qiao", "巧桥瞧敲悄乔侨翘"), ("qie", "且切窃茄怯妾"), ("qin", "亲勤琴秦侵禽钦"), ("qing", "请情清轻青庆晴倾"),
    ("qiong", "穷琼穹"), ("qiu", "求球秋邱囚丘"), ("qu", "去取区曲趣屈渠驱娶"), ("quan", "全权劝圈泉拳犬"), ("que", "却确缺雀瘸鹊"), ("qun", "群裙"),
    ("xi", "系西习喜细希洗戏息吸席析"), ("xia", "下夏吓虾峡侠瞎霞"), ("xian", "现先线显鲜险限县闲献陷嫌"),
    ("xiang", "想向像相项香乡箱享响详相"), ("xiao", "小笑校效消晓销肖孝萧"), ("xie", "写些谢鞋协血斜歇携蟹泄"),
    ("xin", "新心信辛欣薪芯"), ("xing", "行性型形星兴醒幸姓刑"), ("xiong", "兄胸雄凶熊汹"), ("xiu", "修休秀袖绣锈"),
    ("xu", "需许续须虚序徐蓄绪叙"), ("xuan", "选宣悬旋玄轩"), ("xue", "学雪血削穴靴薛"), ("xun", "训寻迅讯询巡循熏"),
    ("zha", "炸扎眨渣诈榨闸"), ("zhai", "摘宅窄债寨斋"), ("zhan", "站战展占沾斩粘盏"), ("zhang", "张长章涨掌丈帐障账"),
    ("zhao", "找照招着赵朝罩召"), ("zhe", "这着者折哲遮"), ("zhei", "这"), ("zhen", "真阵镇震针珍诊枕"), ("zheng", "正证政整争征挣郑症睁"),
    ("zhi", "只之直知制指至值支职志纸置"), ("zhong", "中种重众终钟忠仲肿"), ("zhou", "周州洲粥皱骤昼宙"),
    ("zhu", "主住注助著猪逐祝竹珠驻"), ("zhua", "抓爪"), ("zhuai", "拽"), ("zhuan", "转专赚传砖撰"), ("zhuang", "装状壮撞庄妆"),
    ("zhui", "追坠缀锥赘"), ("zhun", "准"), ("zhuo", "桌捉卓着啄琢"),
    ("cha", "查差茶插察叉岔"), ("chai", "拆差柴豺钗"), ("chan", "产缠颤馋蝉铲"), ("chang", "长场常唱厂尝肠偿畅"),
    ("chao", "超朝吵炒潮抄巢"), ("che", "车彻撤扯澈"), ("chen", "陈沉晨趁衬称尘"), ("cheng", "成城程称承乘诚呈撑惩"),
    ("chi", "吃持迟尺赤池齿翅斥"), ("chong", "冲重充虫崇宠"), ("chou", "抽丑愁仇筹臭酬"), ("chu", "出处除初楚触础储畜"),
    ("chua", "欻"), ("chuai", "揣踹"), ("chuan", "传穿船川串喘"), ("chuang", "创窗床闯疮幢"), ("chui", "吹垂锤炊捶"),
    ("chun", "春纯唇蠢醇淳"), ("chuo", "戳绰辍"),
    ("sha", "杀沙啥傻纱厦刹"), ("shai", "晒筛色"), ("shan", "山善闪扇删衫杉陕擅膳"), ("shang", "上商伤尚赏裳"),
    ("shao", "少绍烧稍勺哨捎"), ("she", "设社射舍蛇涉摄舌"), ("shei", "谁"), ("shen", "什身深神甚申伸审肾慎"),
    ("sheng", "生声省胜升剩圣盛绳"), ("shi", "是时事使实十世市式师试识视"), ("shou", "手收受首授守售寿瘦兽"),
    ("shu", "数书属树术输熟述束鼠叔舒"), ("shua", "刷耍"), ("shuai", "帅摔衰甩率"), ("shuan", "栓拴涮闩"), ("shuang", "双爽霜孀"),
    ("shui", "水谁睡税"), ("shun", "顺瞬舜吮"), ("shuo", "说硕朔烁"),
    ("ran", "然燃染冉"), ("rang", "让嚷壤攘瓤"), ("rao", "绕扰饶娆"), ("re", "热惹"), ("ren", "人认任仁忍刃韧"), ("reng", "仍扔"), ("ri", "日"),
    ("rong", "容荣融溶绒熔蓉榕"), ("rou", "肉柔揉蹂"), ("ru", "如入乳辱儒汝蠕"), ("ruan", "软阮"), ("rui", "瑞锐蕊睿"), ("run", "润闰"),
    ("ruo", "若弱"), ("rua", "挼"),
    ("za", "杂砸咱扎匝"), ("zai", "在再载灾栽宰"), ("zan", "咱赞暂攒"), ("zang", "脏葬藏赃"), ("zao", "早造遭燥糟澡枣"),
    ("ze", "则责择泽啧"), ("zei", "贼"), ("zen", "怎"), ("zeng", "增赠曾憎"), ("zi", "子自字资紫姿滋仔籽"), ("zong", "总宗综纵踪棕"),
    ("zou", "走奏揍邹"), ("zu", "组族足祖阻租"), ("zuan", "钻纂"), ("zui", "最嘴罪醉"), ("zun", "尊遵樽"), ("zuo", "做作坐左座昨"),
    ("ca", "擦嚓"), ("cai", "才菜财材采猜彩裁踩"), ("can", "参餐残惨灿蚕"), ("cang", "藏仓苍舱沧"), ("cao", "草操曹槽糙"),
    ("ce", "测策册侧厕"), ("cen", "参岑"), ("ceng", "层曾蹭"), ("ci", "次此词辞刺磁瓷慈"), ("cong", "从聪丛匆葱"), ("cou", "凑"),
    ("cu", "粗促醋簇猝"), ("cuan", "窜蹿篡"), ("cui", "催脆翠崔摧粹"), ("cun", "存村寸"), ("cuo", "错措挫搓撮"),
    ("sa", "撒洒萨仨飒"), ("sai", "赛塞腮"), ("san", "三散伞叁"), ("sang", "桑嗓丧"), ("sao", "扫嫂骚搔"), ("se", "色涩瑟塞"),
    ("sen", "森"), ("seng", "僧"), ("si", "四死思斯司似私丝寺"), ("song", "送松宋颂耸诵"), ("sou", "搜艘嗖馊"),
    ("su", "速素诉宿苏俗塑肃"), ("suan", "算酸蒜"), ("sui", "虽岁随碎遂隧"), ("sun", "孙损笋"), ("suo", "所锁缩索琐唆"),
    ("ya", "呀压牙亚鸭雅芽崖哑押"), ("yan", "眼言研严颜验烟沿演延盐炎"), ("yang", "样养阳洋羊杨扬仰氧"),
    ("yao", "要药摇腰咬遥邀耀妖"), ("ye", "也夜业页叶爷野液耶"), ("yi", "一以已意义亿易医依衣移疑音"),
    ("yin", "因音引银印饮隐阴"), ("ying", "应影英营硬迎赢映"), ("yo", "哟唷"), ("yong", "用永勇拥涌泳庸"),
    ("you", "有又由友右游油优幽忧"), ("yu", "与于语鱼雨遇预育玉域宇余"), ("yuan", "元原员远院愿园圆源缘"),
    ("yue", "月越约乐阅跃岳"), ("yun", "云运允晕孕韵匀"),
    ("wa", "哇挖娃瓦袜蛙"), ("wai", "外歪"), ("wan", "完玩晚万湾碗挽弯"), ("wang", "往网忘王望旺汪枉"),
    ("wei", "为位未委围维味卫微伟危喂"), ("wen", "问文闻稳温吻纹"), ("weng", "翁嗡瓮"), ("wo", "我握窝卧沃蜗"), ("wu", "无五物务午舞误屋武雾"),
];

const PY_WORDS: &[(&str, &[&str])] = &[
    ("nihao", &["你好"]), ("nihaoshijie", &["你好世界"]), ("nimen", &["你们"]), ("women", &["我们"]),
    ("tamen", &["他们", "她们", "它们"]), ("dajia", &["大家"]), ("ziji", &["自己"]),
    ("shenme", &["什么"]), ("zenme", &["怎么"]), ("zenmeyang", &["怎么样"]), ("weishenme", &["为什么"]),
    ("nali", &["哪里", "那里"]), ("zheli", &["这里"]), ("zhege", &["这个"]), ("nage", &["那个", "哪个"]),
    ("xianzai", &["现在"]), ("jintian", &["今天"]), ("mingtian", &["明天"]), ("zuotian", &["昨天"]),
    ("shijian", &["时间", "事件", "实践"]), ("meiyou", &["没有"]), ("yijing", &["已经"]), ("keyi", &["可以"]),
    ("yinggai", &["应该"]), ("juede", &["觉得"]), ("zhidao", &["知道", "指导"]), ("xihuan", &["喜欢"]),
    ("xuyao", &["需要"]), ("suoyi", &["所以"]), ("yinwei", &["因为"]), ("danshi", &["但是"]),
    ("ruguo", &["如果"]), ("haishi", &["还是"]), ("jiushi", &["就是"]), ("bushi", &["不是"]),
    ("xiexie", &["谢谢"]), ("bukeqi", &["不客气"]), ("meiguanxi", &["没关系"]), ("duibuqi", &["对不起"]),
    ("haode", &["好的"]), ("shoudao", &["收到"]), ("mingbai", &["明白"]), ("meiwenti", &["没问题"]),
    ("jianpan", &["键盘"]), ("shurufa", &["输入法"]), ("pingmu", &["屏幕"]), ("pingban", &["平板"]),
    ("diannao", &["电脑"]), ("xitong", &["系统"]), ("shezhi", &["设置"]), ("wenjian", &["文件"]),
    ("wangluo", &["网络"]), ("yinliang", &["音量"]), ("liangdu", &["亮度"]), ("zhongwen", &["中文"]),
    ("yingwen", &["英文"]), ("pinyin", &["拼音"]), ("houxuan", &["候选"]), ("biaoqing", &["表情"]),
    ("fuhao", &["符号"]), ("kongge", &["空格"]), ("huiche", &["回车"]), ("qiehuan", &["切换"]),
    ("chuping", &["触屏"]), ("shoushi", &["手势"]), ("suoping", &["锁屏"]), ("jiesuo", &["解锁"]),
    ("shuaji", &["刷机"]), ("neihe", &["内核"]), ("dianchi", &["电池"]), ("dianliang", &["电量"]),
    ("chongdian", &["充电"]), ("lanya", &["蓝牙"]), ("wuxian", &["无线"]), ("mima", &["密码"]),
    ("denglu", &["登录"]), ("jietu", &["截图"]), ("xiangji", &["相机"]), ("erji", &["耳机"]),
    ("shubiao", &["鼠标"]), ("xianshiqi", &["显示器"]), ("shuaxinlv", &["刷新率"]),
    ("chuangkou", &["窗口"]), ("zhuomian", &["桌面"]), ("yingyong", &["应用"]), ("caidan", &["菜单"]),
    ("kongzhi", &["控制"]), ("tongzhi", &["通知"]), ("gongzuoqu", &["工作区"]), ("zhongduan", &["终端"]),
    ("liulanqi", &["浏览器"]), ("daima", &["代码"]), ("kaifa", &["开发"]), ("bianyi", &["编译"]),
    ("yunxing", &["运行"]), ("mingling", &["命令"]), ("baocun", &["保存"]), ("fuzhi", &["复制"]),
    ("zhantie", &["粘贴"]), ("jianqieban", &["剪贴板"]), ("sousuo", &["搜索"]), ("bianqian", &["便签"]),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinyin_search_and_segmentation_work() {
        assert!(matches_app_query("终端 (Foot)", "foot", "zd"));
        assert!(matches_app_query("系统设置", "gnome-control-center", "xtsz"));
        assert!(matches_app_query("Firefox 浏览器", "firefox", "llq"));
        let (pre, cands) = lookup_pinyin_candidates("nihaoshijie");
        assert_eq!(pre, "ni'hao'shi'jie");
        assert_eq!(cands.first().map(|s| s.as_str()), Some("你好世界"));
    }
}
