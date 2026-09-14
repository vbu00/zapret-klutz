//! Фоновая проверка связи и самолечение.
//!
//! Стратегия считается «просевшей», когда какой-нибудь ключевой сервис
//! (Discord, YouTube) перестал отвечать целиком. Одна неудачная проверка —
//! обычно просто сетевая икота, поэтому требуем несколько подряд, прежде чем
//! что-то делать.

use tauri::{AppHandle, Emitter, Manager};

use crate::probe::PathVerdict;
use crate::state::{save_state, AppState, HealEntry};
use crate::targets;
use crate::winws;

fn now_ms() -> u64 {
    // Часы пользователя могут стоять до 1970-го — это не повод падать.
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// К какому сервису относится цель. Признак — имя, а его пользователь может
/// переименовать, поэтому ниже есть запасной путь.
fn service_of(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.starts_with("discord") {
        Some("discord")
    } else if n.starts_with("youtube") {
        Some("youtube")
    } else {
        None
    }
}

/// Просадка — когда хоть один ключевой сервис перестал отвечать целиком.
///
/// Считать долей от общего числа хостов нельзя: у Discord их четыре, у
/// YouTube три, и условие «ответило меньше половины» пропускало «Discord
/// лёг весь» (3 из 7), но никогда не срабатывало на «YouTube лёг весь»
/// (4 из 7) — переключение было асимметричным.
fn is_degraded(results: &[targets::TargetResult]) -> bool {
    let mut groups: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for r in results {
        // Отказ самого сервера (его 403, его сертификат) — не блокировка, и
        // никакая стратегия его не чинит. Такая цель не голосует ни за, ни
        // против: считать её провалом значило бы гонять перебор впустую.
        if r.verdict == PathVerdict::Server {
            continue;
        }
        let e = groups.entry(service_of(&r.name).unwrap_or("прочие")).or_insert((0, 0));
        e.1 += 1;
        if r.ok {
            e.0 += 1;
        }
    }
    groups.values().any(|(ok, total)| *total > 0 && *ok == 0)
}

/// Есть ли вообще смысл менять стратегию.
///
/// Если каждая упавшая цель упала с вердиктом «режут адрес», перебирать
/// нечего: пакетные техники блок по IP не обходят в принципе. Раньше
/// самолечение этого не знало и честно сжигало весь рейтинг, меняя стратегию
/// заодно и для всех остальных целей.
/// Cutoff сюда добавлен не для симметрии: этот вердикт прямо говорит, что
/// имя уже проехало и рукопожатие состоялось, — перебирать способы это имя
/// спрятать бессмысленно.
fn desync_can_help(results: &[targets::TargetResult]) -> bool {
    let failed: Vec<_> = results.iter().filter(|r| !r.ok && r.verdict != PathVerdict::Server).collect();
    if failed.is_empty() {
        return false;
    }
    // Блок по адресу и юридический 451 стратегией не лечатся. «Не измерено»
    // не мешает попробовать: запрещать перебор из-за неудавшегося замера
    // было бы хуже лишней попытки.
    failed
        .iter()
        .any(|r| !matches!(r.verdict, PathVerdict::Ip | PathVerdict::Legal))
}

pub fn start(app: AppHandle) {
    std::thread::spawn(move || loop {
        // Проверяем сразу, а не после первого сна: иначе полминуты после
        // запуска приложение вообще не знает состояния связи.
        tick(&app);
        let interval = {
            let state = app.state::<AppState>();
            let p = state.persisted.lock().unwrap();
            p.auto_switch.as_ref().map(|a| a.interval_sec).unwrap_or(30)
        };
        std::thread::sleep(std::time::Duration::from_secs(interval.clamp(10, 600)));
    });
}

fn tick(app: &AppHandle) {
    let state = app.state::<AppState>();

    // Во время прогона тестов стратегия меняется каждые несколько секунд —
    // мерить в этот момент бессмысленно и вредно.
    if *state.testing.lock().unwrap() || crate::gamescan::scan_busy() {
        return;
    }
    if !winws::is_winws_running() {
        // Обход выключен — серия самолечения кончилась вместе с ним.
        // Без сброса накопленный список испробованного доживал до
        // следующего запуска и приводил к преждевременному «сдалось».
        state.healing_attempts.lock().unwrap().clear();
        *state.heal_exhausted.lock().unwrap() = false;
        *state.last_check.lock().unwrap() = None;
        state.last_targets.lock().unwrap().clear();
        *state.last_check_at.lock().unwrap() = 0;
        *state.degraded_ticks.lock().unwrap() = 0;
        crate::tray::refresh(app);
        return;
    }

    let list = {
        let p = state.persisted.lock().unwrap();
        p.game_targets.clone().unwrap_or_else(targets::default_targets)
    };
    // Если пользователь переименовал все цели, признака «ключевая» не
    // остаётся. Раньше на этом месте был ранний return — и мониторинг тихо
    // умирал навсегда: трей застывал на старых данных, самолечение не
    // срабатывало, сообщения об этом не было.
    //
    // Отбирать по признаку можно, только если ОБА сервиса опознались.
    // Иначе выходило хуже незаметного: переименовали цели Discord, YouTube
    // остались стандартными — core непустой, всё выглядит рабочим, а падение
    // Discord не замечается вовсе.
    let есть = |s: &str| list.iter().any(|t| service_of(&t.name) == Some(s));
    let mut core: Vec<_> = if есть("discord") && есть("youtube") {
        list.iter().filter(|t| service_of(&t.name).is_some()).cloned().collect()
    } else {
        Vec::new()
    };
    if core.is_empty() {
        core = list;
    }
    if core.is_empty() {
        return;
    }
    // Снимок ДО проверки. Она занимает секунды, и за это время стратегию
    // могли переключить — из трея, из окна или самолечением. Такой результат
    // измерял ПРЕЖНЮЮ стратегию, и закреплять его за нынешней нельзя.
    let measured = state.persisted.lock().unwrap().active_config.clone();
    let results = targets::check_targets(&core);
    let attributable = {
        let now = state.persisted.lock().unwrap().active_config.clone();
        measured.is_some() && measured == now
    };
    // Проверка заняла секунды. За это время мог начаться прогон тестов —
    // он крутит стратегию каждые несколько секунд, и всё, что ниже, от
    // записи результатов до переключения, только мешало бы ему.
    if *state.testing.lock().unwrap() || crate::gamescan::scan_busy() {
        return;
    }
    let ok = results.iter().filter(|r| r.ok).count();
    let total = results.len();
    *state.last_check.lock().unwrap() = Some((ok, total));
    *state.last_targets.lock().unwrap() = results.iter().map(|r| (r.name.clone(), r.ok, r.ms)).collect();
    *state.last_check_at.lock().unwrap() = now_ms();
    crate::tray::refresh(app);

    // Результат, снятый при ДРУГОЙ стратегии, не повод менять нынешнюю.
    // Проверка уже была, но пользовалась ею только запись «эта работает»;
    // само переключение шло и на чужих данных.
    if !attributable {
        return;
    }

    let degraded = is_degraded(&results);
    if !degraded {
        *state.degraded_ticks.lock().unwrap() = 0;
        state.healing_attempts.lock().unwrap().clear();
        *state.heal_exhausted.lock().unwrap() = false;
        if attributable {
            if let Some(name) = measured {
                remember_working(app, &state, &results, &name);
            }
        }
        return;
    }

    // Блок по адресу стратегией не лечится. Считаем тики (человек видит
    // просадку), но перебор не запускаем и говорим об этом один раз.
    if !desync_can_help(&results) {
        *state.degraded_ticks.lock().unwrap() = 0;
        if !*state.heal_exhausted.lock().unwrap() {
            *state.heal_exhausted.lock().unwrap() = true;
            // Текст по фактическому вердикту. Раньше здесь стояло одно
            // «режут адрес» на оба случая, и цель с ответом 451 получала
            // рассказ про нейтральное имя, которого ей не задавали.
            let (title, body) = if results
                .iter()
                .filter(|r| !r.ok)
                .all(|r| r.verdict == PathVerdict::Legal)
            {
                (
                    "Заблокировано по закону",
                    "Сервер отвечает 451 «недоступно по юридическим причинам». Это не DPI, \
                     и сменой стратегии такое не лечится.",
                )
            } else {
                (
                    "Блокировка по адресу",
                    "Цели не отвечают и с нейтральным именем на тот же адрес — режут адрес, \
                     а не имя. Обход этого не обойдёт: поможет другой адрес или туннель.",
                )
            };
            crate::notify::send_from(app, title, body);
        }
        return;
    }

    let (enabled, threshold) = {
        let p = state.persisted.lock().unwrap();
        let a = p.auto_switch.clone().unwrap_or_default();
        (a.enabled, a.threshold)
    };
    let ticks = {
        let mut t = state.degraded_ticks.lock().unwrap();
        *t = t.saturating_add(1);
        *t
    };
    if ticks < threshold || !enabled {
        return;
    }
    attempt_switch(app);
}

/// Настоящий успех: хотя бы одна цель ответила сама, а не «сервер жив, но
/// отказал». Отказ сервера (его 403, его сертификат) доказывает, что жив
/// сервер, а не что работает обход, — закреплять стратегию на таком
/// основании нельзя.
fn has_real_success(results: &[targets::TargetResult]) -> bool {
    results.iter().any(|r| r.ok && r.verdict == PathVerdict::Ok)
}

/// На что переключаться.
///
/// Сначала то, что здесь УЖЕ работало: рейтинг снят прогоном, возможно, много
/// дней назад и в другой сетевой обстановке, а подтверждённый замером конфиг —
/// знание свежее и про эту самую сеть. Дальше идёт рейтинг.
///
/// Кандидат обязан лежать в списке конфигов релиза: имена приходят из файла
/// результатов, который пишет чужой скрипт, и строка вида «..\\other.bat»
/// увела бы запуск за пределы папки.
fn pick_next(
    current: Option<&str>,
    working: Option<&str>,
    ranked: &[String],
    tried: &[String],
    configs: &[String],
) -> Option<String> {
    let годится = |c: &str| {
        Some(c) != current && !tried.iter().any(|t| t == c) && configs.iter().any(|x| x == c)
    };
    if let Some(w) = working.filter(|w| годится(w)) {
        return Some(w.to_string());
    }
    ranked.iter().find(|c| годится(c)).cloned()
}

/// Запоминает стратегию, на которой проверка прошла чисто.
///
/// Требуем НАСТОЯЩЕГО успеха хотя бы по одной цели: «сервер ответил сам»
/// (его 403, его сертификат) доказывает, что жив сервер, а не что работает
/// обход, и закреплять стратегию на таком основании нельзя.
///
/// Пишем только при смене значения — иначе диск дёргался бы каждые полминуты.
fn remember_working(
    app: &AppHandle,
    state: &tauri::State<AppState>,
    results: &[targets::TargetResult],
    name: &str,
) {
    if !has_real_success(results) {
        return;
    }
    let changed = {
        let mut p = state.persisted.lock().unwrap();
        let same = p.working_config.as_deref() == Some(name);
        p.working_at = Some(now_ms());
        if same {
            false
        } else {
            p.working_config = Some(name.to_string());
            true
        }
    };
    if changed {
        save_state(app, state);
    }
}

/// Переключается на следующую стратегию из рейтинга последнего прогона,
/// пропуская те, что уже пробовали в этой серии.
fn attempt_switch(app: &AppHandle) {
    let state = app.state::<AppState>();
    let root = match state.persisted.lock().unwrap().root_path.clone() {
        Some(r) => std::path::PathBuf::from(r),
        None => return,
    };

    let (current, working) = {
        let p = state.persisted.lock().unwrap();
        (p.active_config.clone(), p.working_config.clone())
    };
    let tried = state.healing_attempts.lock().unwrap().clone();
    // Имена приходят из файла результатов, который пишет чужой скрипт.
    // Без проверки строка вида «..\\other.bat» увела бы apply_config за
    // пределы папки релиза.
    let configs = crate::release::list_configs(&root);
    let next = pick_next(
        current.as_deref(),
        working.as_deref(),
        &latest_ranking(&root),
        &tried,
        &configs,
    );

    let Some(next) = next else {
        // Перепробовали всё — молотить дальше бессмысленно, но и выключать
        // самолечение нельзя: раньше здесь стояло `a.enabled = false` с
        // записью на диск, и получасовой обрыв связи навсегда гасил чужую
        // настройку. Список испробованного и так держит нас в покое: пока
        // связь не вернётся, следующей стратегии не найдётся. Как только
        // цели снова ответят, tick() очистит его сам.
        if !tried.is_empty() && !*state.heal_exhausted.lock().unwrap() {
            *state.heal_exhausted.lock().unwrap() = true;
            // В журнал это писалось только уведомлением: кто его пропустил,
            // потом не находил в истории никакого следа — почему самолечение
            // молчит. Интерфейс такую запись рисовать умел давно, а бэкенд
            // её не создавал ни разу.
            {
                let mut p = state.persisted.lock().unwrap();
                let log = p.heal_log.get_or_insert_with(Vec::new);
                log.push(HealEntry {
                    at: now_ms(),
                    kind: "gave-up".into(),
                    from: current.clone(),
                    to: None,
                    ok: false,
                    tried_count: tried.len() as u32,
                });
                if log.len() > 50 {
                    let cut = log.len() - 50;
                    log.drain(0..cut);
                }
            }
            save_state(app, &state);
            crate::notify::send_from(
                app,
                "Самолечение перебрало все стратегии",
                "Ни одна из последнего прогона не вернула связь. Похоже, дело не в стратегии — проверь интернет или прогони тесты заново.",
            );
        }
        return;
    };

    state.healing_attempts.lock().unwrap().push(next.clone());
    *state.degraded_ticks.lock().unwrap() = 0;

    let applied = apply_config(app, &next);
    // В журнал попадает и неудача: раньше запись делалась только в ветке
    // успеха и всегда с ok: true, поэтому серия провалившихся переключений
    // выглядела для пользователя полной тишиной.
    {
        let mut p = state.persisted.lock().unwrap();
        let log = p.heal_log.get_or_insert_with(Vec::new);
        log.push(HealEntry {
            at: now_ms(),
            kind: "switch".into(),
            from: current.clone(),
            to: Some(next.clone()),
            ok: applied.is_ok(),
            tried_count: 0,
        });
        if log.len() > 50 {
            let cut = log.len() - 50;
            log.drain(0..cut);
        }
        drop(p);
        save_state(app, &state);
    }

    if applied.is_ok() {
        crate::notify::send_from(
            app,
            "Переключился на другую стратегию",
            &format!(
                "{}Включена {}.",
                current
                    .as_ref()
                    .map(|c| format!("{} перестала работать. ", c.trim_end_matches(".bat")))
                    .unwrap_or_default(),
                next.trim_end_matches(".bat")
            ),
        );
        let _ = app.emit(
            "auto-switched",
            serde_json::json!({ "from": current, "to": next }),
        );
    }
}

/// Включает конфиг тем же способом, каким сейчас работает обход: службой,
/// если стоит служба, иначе прямым запуском winws. Общий путь для
/// самолечения и меню «Переключить на» в трее.
/// Переключение стратегии — по одному за раз.
///
/// Звать `apply_config` могут трое сразу: окно, меню в трее и самолечение.
/// Без замка два вызова поднимали по своему winws, а `active_config`
/// доставался тому, кто записал последним, — показанная стратегия и
/// работающая расходились.
static APPLY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn apply_config(app: &AppHandle, name: &str) -> Result<(), String> {
    let _guard = APPLY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let state = app.state::<AppState>();
    let (root, as_service) = {
        let p = state.persisted.lock().unwrap();
        (p.root_path.clone(), p.installed_as_service)
    };
    let root = std::path::PathBuf::from(root.ok_or("Сначала загрузи релиз zapret.")?);
    // Та же проверка, что делает кнопка в окне. Раньше здесь стояло только
    // членство в списке — а через эту функцию идут самолечение и трей, то
    // есть ровно те пути, где человек имя не набирал и глазами не видел.
    // Список читается с диска, и что в нём лежит, задаёт чужой архив.
    crate::commands::checked_config(&root, name)?;
    if as_service {
        crate::service::install_service(&root, name).map_err(|e| e.to_string())?;
    } else {
        if crate::service::service_conflict() {
            return Err("Установлена служба Windows «zapret» — сначала сними её.".into());
        }
        winws::spawn_winws(app, &root, name).map_err(|e| e.to_string())?;
    }
    {
        let mut p = state.persisted.lock().unwrap();
        p.active_config = Some(name.to_string());
        p.started_at = Some(now_ms());
    }
    save_state(app, &state);
    crate::tray::refresh(app);
    Ok(())
}

/// Рейтинг из свежайшего файла результатов тестов.
pub fn latest_ranking(root: &std::path::Path) -> Vec<String> {
    latest_ranking_scored(root).into_iter().map(|(c, _)| c).collect()
}

/// Тот же рейтинг вместе с долей ответивших целей — меню трея показывает её
/// рядом с именем, как список конфигов в окне.
pub fn latest_ranking_scored(root: &std::path::Path) -> Vec<(String, f64)> {
    // Свежайший — по времени изменения. Здесь ошибиться дороже всего:
    // по этому рейтингу самолечение выбирает, на что переключаться.
    let Some(path) = crate::tests::newest_result_file(root) else {
        return vec![];
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let (mut rows, dpi) = crate::tests::parse_results(&text);
    rows.sort_by(|a, b| crate::tests::rank_desc(a, b, dpi));
    // Проверка «как у приложений» могла поднять конфиг выше лучшего по тестам:
    // зелёный в тестах, на котором Discord висит, первым не ставим.
    if let Some(best) = crate::tests::app_best(&text) {
        let bare = best.trim_end_matches(".bat");
        if let Some(i) = rows.iter().position(|r| r.config.trim_end_matches(".bat") == bare) {
            let r = rows.remove(i);
            rows.insert(0, r);
        }
    }
    rows.into_iter()
        .map(|r| {
            let score = r.score(dpi);
            let config = if r.config.to_lowercase().ends_with(".bat") {
                r.config
            } else {
                format!("{}.bat", r.config)
            };
            (config, score)
        })
        .collect()
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::targets::TargetResult;

    fn t(name: &str, ok: bool) -> TargetResult {
        mk(name, ok, if ok { PathVerdict::Ok } else { PathVerdict::Sni })
    }

    fn mk(name: &str, ok: bool, verdict: PathVerdict) -> TargetResult {
        TargetResult {
            name: name.into(),
            host: "h".into(),
            port: 443,
            ok,
            ms: 1,
            reason: None,
            probe: "http",
            code: if ok { crate::probe::FailureCode::Ok } else { crate::probe::FailureCode::TlsFailed },
            verdict,
            why: None,
        }
    }

    /// Стандартные ключевые цели: четыре Discord и три YouTube.
    fn целиком(discord_ok: bool, youtube_ok: bool) -> Vec<TargetResult> {
        let mut v: Vec<_> = (1..=4).map(|i| t(&format!("Discord {i}"), discord_ok)).collect();
        v.extend((1..=3).map(|i| t(&format!("YouTube {i}"), youtube_ok)));
        v
    }

    #[test]
    fn просадка_симметрична_по_сервисам() {
        assert!(is_degraded(&целиком(false, true)), "Discord лёг целиком — это просадка");
        // Ровно этот случай старое условие ok*2 < total не ловило никогда:
        // 4 живых из 7, 8 < 7 ложно.
        assert!(is_degraded(&целиком(true, false)), "YouTube лёг целиком — тоже просадка");
        assert!(!is_degraded(&целиком(true, true)), "всё отвечает — не просадка");
        assert!(is_degraded(&целиком(false, false)));
    }

    #[test]
    fn старое_условие_действительно_пропускало_youtube() {
        let r = целиком(true, false);
        let ok = r.iter().filter(|x| x.ok).count();
        assert_eq!(ok, 4);
        assert!(ok * 2 >= r.len(), "старое условие тут молчало");
    }

    #[test]
    fn одна_упавшая_цель_из_сервиса_не_считается_просадкой() {
        let mut r = целиком(true, true);
        r[0].ok = false;
        assert!(!is_degraded(&r), "одна икота из четырёх — не повод переключаться");
    }

    #[test]
    fn сервис_определяется_без_учёта_регистра() {
        assert_eq!(service_of("Discord Main"), Some("discord"));
        assert_eq!(service_of("YOUTUBE Web"), Some("youtube"));
        assert_eq!(service_of("Steam"), None);
    }

    #[test]
    fn юридический_блок_перебор_не_запускает() {
        // 451 приходит от самого сервера по требованию закона — стратегия
        // такое не лечит, как и блок по адресу.
        let r = vec![mk("Discord Main", false, PathVerdict::Legal)];
        assert!(!desync_can_help(&r));
        // А вот «не измерено» перебору не мешает: лишняя попытка дешевле
        // отказа из-за неудавшегося замера.
        let r = vec![mk("Discord Main", false, PathVerdict::Unknown)];
        assert!(desync_can_help(&r));
    }

    #[test]
    fn подтверждённо_рабочая_стратегия_идёт_первой() {
        let configs: Vec<String> = ["a.bat", "b.bat", "c.bat"].iter().map(|s| s.to_string()).collect();
        let ranked: Vec<String> = ["c.bat", "b.bat"].iter().map(|s| s.to_string()).collect();
        // Рейтинг советует c, но b здесь уже работала — берём b.
        assert_eq!(
            pick_next(Some("a.bat"), Some("b.bat"), &ranked, &[], &configs).as_deref(),
            Some("b.bat")
        );
    }

    #[test]
    fn рабочую_не_предлагаем_если_она_и_включена_или_уже_пробована() {
        let configs: Vec<String> = ["a.bat", "b.bat", "c.bat"].iter().map(|s| s.to_string()).collect();
        let ranked: Vec<String> = ["c.bat"].iter().map(|s| s.to_string()).collect();
        // Она же и активна — предлагать её бессмысленно.
        assert_eq!(
            pick_next(Some("b.bat"), Some("b.bat"), &ranked, &[], &configs).as_deref(),
            Some("c.bat")
        );
        // Уже пробовали в этой серии и не помогло.
        let tried = vec!["b.bat".to_string()];
        assert_eq!(
            pick_next(Some("a.bat"), Some("b.bat"), &ranked, &tried, &configs).as_deref(),
            Some("c.bat")
        );
    }

    #[test]
    fn кандидата_нет_в_релизе_не_предлагаем() {
        let configs: Vec<String> = ["a.bat"].iter().map(|s| s.to_string()).collect();
        let ranked: Vec<String> = vec!["..\\чужое.bat".to_string(), "нет-такого.bat".to_string()];
        assert_eq!(pick_next(Some("a.bat"), Some("тоже-нет.bat"), &ranked, &[], &configs), None);
    }

    #[test]
    fn без_рабочей_берём_рейтинг() {
        let configs: Vec<String> = ["a.bat", "c.bat"].iter().map(|s| s.to_string()).collect();
        let ranked: Vec<String> = vec!["c.bat".to_string()];
        assert_eq!(
            pick_next(Some("a.bat"), None, &ranked, &[], &configs).as_deref(),
            Some("c.bat")
        );
    }

    #[test]
    fn закрепляем_только_на_настоящем_успехе() {
        // Ответил сам сервер — обход тут ни при чём.
        assert!(!has_real_success(&[mk("Discord", false, PathVerdict::Server)]));
        // Ни одна цель не ответила.
        assert!(!has_real_success(&[mk("Discord", false, PathVerdict::Sni)]));
        assert!(!has_real_success(&[]));
        // Хотя бы одна ответила по-настоящему.
        assert!(has_real_success(&[
            mk("Discord", false, PathVerdict::Sni),
            mk("YouTube", true, PathVerdict::Ok),
        ]));
    }

    #[test]
    fn отказ_самого_сервера_не_считается_просадкой() {
        // Сервер ответил своим 403 или своим сертификатом. Стратегия этого
        // не чинит, и голосовать за перебор такая цель не должна.
        let r = vec![
            mk("Discord Main", false, PathVerdict::Server),
            mk("Discord CDN", false, PathVerdict::Server),
            mk("YouTube Web", true, PathVerdict::Ok),
        ];
        assert!(!is_degraded(&r), "отказ сервера — не повод переключать стратегию");
        assert!(!desync_can_help(&r), "и перебирать тут нечего");
    }

    #[test]
    fn блок_по_адресу_перебор_не_запускает() {
        let r = vec![
            mk("Discord Main", false, PathVerdict::Ip),
            mk("Discord CDN", false, PathVerdict::Ip),
        ];
        assert!(is_degraded(&r), "просадка есть — человек её видит");
        assert!(!desync_can_help(&r), "но стратегией она не лечится");
    }

    #[test]
    fn блок_по_имени_перебор_запускает() {
        let r = vec![
            mk("Discord Main", false, PathVerdict::Sni),
            mk("Discord CDN", false, PathVerdict::Ip),
        ];
        assert!(desync_can_help(&r), "хоть одна цель режется по имени — пробуем");
    }

    #[test]
    fn всё_отвечает_перебирать_нечего() {
        let r = vec![mk("Discord Main", true, PathVerdict::Ok)];
        assert!(!desync_can_help(&r));
    }

    #[test]
    fn переименованные_цели_попадают_в_одну_группу() {
        // Пользователь переименовал всё — раньше tick() уходил в ранний
        // return и мониторинг умирал молча.
        let r = vec![t("Дискорд", false), t("Ютуб", false)];
        assert!(is_degraded(&r));
        let r = vec![t("Дискорд", true), t("Ютуб", true)];
        assert!(!is_degraded(&r));
    }
}
