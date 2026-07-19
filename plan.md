# ASense v0.3 — Thermal Pilot

> Finální samonosný implementační plán pro v0.3.
>
> Výchozí stav je ASense v0.2.0: malý Acer control center s dynamickými
> firmware profily, fan cascade Kernel PWM → Gaming-WMI → read-only, zoned
> WMI, ENEK5130, nezávislými Battery/APGE endpointy, typed root daemonem a
> observer-neutral NVIDIA discovery.
>
> v0.3 nepřidává další Acer moloch. Využije existující telemetrii a typed
> aktuátory tak, aby notebook předvídal vlastní tepelnou odezvu a reagoval
> dřív než obyčejná fan curve. Zůstane jednoduchým nástrojem pro ventilátory,
> profily a světla.

---

## 1. Produktový výsledek

v0.3 přinese tři věci, které nejsou jen další checkbox:

1. **Thermal Pilot** — předpoví teplotu 10/20/30 sekund dopředu a použije
   existující profil/fan backend dřív, než teplota přestřelí.
2. **Workload intent bez špehování** — explicitní Steam/GameMode session a
   okamžité desktop signály rozliší hru, video, práci a idle bez ukládání
   kláves, procesů nebo historie aktivity.
3. **Fan commissioning a Cooling Fingerprint** — krátký řízený test zjistí
   vazbu CPU/GPU/třetího ventilátoru, odezvu a změnu účinnosti chlazení
   konkrétního kusu.

Každodenní použitelnost doplní:

- tray ikona a autostart;
- asense ctl a asense run;
- automatický profil podle AC/baterie nebo explicitní session;
- GPU Dynamic Boost/TGP readiness;
- další užitečná hwmon/RAPL čidla;
- krátké teplotní notifikace;
- pojmenované lighting presets;
- GUI vytvoření a předání sanitizovaného support reportu;
- dokumentovaný malý API contract pro vlastní GUI.

Veřejná produktová věta:

> ASense Thermal Pilot se naučí tepelnou odezvu konkrétního notebooku a
> používá jeho skutečné firmware/fan capabilities s předstihem — lokálně,
> bez cloudu, bez chatbota a bez historie toho, co uživatel dělal.

---

## 2. Tvrdý rozsah a LOC limit

Celý v0.3 přírůstek včetně testů a instalační integrace:

    cíl         3 400–4 700 LOC
    hard limit  5 000 LOC

Do limitu se počítá produkční Rust, testy, fixtures, shell/desktop integrace,
dokumentace nových příkazů i každý trainer/export skript, který se commitne do
ASense. Pouze jednorázový lokální experiment, který v repu ani release není,
se nepočítá. Výjimkou z LOC je samotný binární model asset.

Při překročení se ořezává v tomto pořadí:

1. kosmetika notifikací;
2. více než čtyři lighting presety;
3. rozšířené grafy Cooling Fingerprint;
4. širší sada doplňkových čidel.

Neodkládá se tray, Steam/GameMode session, Thermal Pilot Observe/Active, tiny
RWKV runtime, fan commissioning, privacy invarianty ani support-report
tlačítko.

---

## 3. Hard non-goals

Nevznikne:

- AI chatbot, Ollama nebo model manager;
- procesní/task manager;
- ukládání názvů procesů, oken, her nebo médií;
- keylogger nebo historie kláves;
- audio capture, titulky, názvy skladeb nebo obsah zvuku;
- síťová historie, cílové adresy nebo packet inspection;
- cloud telemetry;
- automatický upload support reportu bez náhledu a souhlasu;
- persistentní worldline všech sekund uživatelova dne;
- obecný plugin marketplace;
- nový raw EC/WMI backend;
- softwarový RGB animation engine;
- generický manager jiných výrobců;
- další root služba;
- druhý fan scheduler vedle dnešní fan-session cesty;
- 4-node fyzikální doktorát, counterfactual strom o desítkách větví nebo
  enterprise policy/evidence systém.

---

## 4. Architektura

    observer-neutral telemetrie
            +
    explicitní workload session / okamžitý Context Pulse
            +
    fan commissioning parametry
            ↓
    malý normalizovaný feature frame
            ↓
    tiny RWKV forecast + jednoduchý termální baseline
            ↓
    deterministický Thermal Pilot
            ↓
    existující typed profile/fan/NVML commands
            ↓
    existující readback, lease a Auto/Maximum recovery

Model nikdy neposílá WMI ID, PWM cestu, HID packet ani privilegovaný příkaz.
Vrací pouze predikci. O konkrétní akci rozhoduje malý deterministický
controller z live capabilities.

Vlastnictví je záměrně jednoduché:

- dlouho běžící neprivilegovaný GUI/tray proces vlastní desktop context,
  RWKV stav, forecast a rozhodovací loop Pilota;
- asensed vlastní jen typed hardware mutace, generation lease, readback a
  dnešní recovery;
- skrytí okna tray proces neukončí a Pilot může pokračovat;
- skutečný Quit zavře control session, čímž se aktivní Manual/Pilot fan stav
  vrátí dnešní fail-safe cestou do Auto.

Root daemon nemusí a nesmí číst uživatelský MPRIS, idle ani session D-Bus.

Single-instance desktop owner má vlastní neprivilegovaný socket
`$XDG_RUNTIME_DIR/asense-ui.sock` s režimem 0600. Používá jej pouze
`asense ctl pilot` a předání session goal z `asense run`; profily, ventilátory
a ostatní hardware commands dál míří přímo do `asensed`. Když desktop owner
neběží, Pilot příkaz spustí stejnou binárku jednou jako `asense --background`,
provede jeden bounded reconnect a jinak vrátí pravdivé
`pilot-owner-unavailable`. Nevzniká druhý daemon ani druhý hardware API.

---

## 5. Observer-neutral telemetry

v0.2 RTD3 pravidla se nesmějí obejít kvůli Pilotovi:

- suspendovaná NVIDIA GPU se kvůli dashboardu ani modelu neotevírá přes NVML;
- sleeping GPU má live hodnoty None, nikoli zastaralý snapshot;
- explicitní uživatelský GPU write smí GPU otevřít po dobu transakce;
- runtime-status polling je pasivní;
- model umí chybějící GPU feature masku;
- retry nesmí sám udržovat GPU aktivní;
- tray bez otevřeného dashboardu nevytváří live NVML session.

### Doplňující čidla

Enumerovat pouze read-only Linux ABI a zachovat label/source:

- všechna validní tempN_input z Acer hwmon;
- NVMe composite/controller temperature;
- RAM/SPD jc42, pokud ji kernel exportuje;
- PCH/VRM/chassis/skin/ACPI thermal zone se srozumitelným labelem;
- CPU package energy/power z RAPL;
- battery voltage/current/power z power_supply;
- celkové disk I/O tempo z /proc/diskstats;
- celkové síťové RX/TX tempo z /proc/net/dev.

Neznámé čidlo se zobrazí v Advanced pod kernelovým labelem, ale nevstoupí do
řízení, dokud nemá stabilní rozsah. Síťová feature je jen okamžitá rychlost;
žádná rozhraní, IP ani cíle se neukládají.

---

## 6. Context Pulse: záměr bez historie

Context není klasifikace uživatele. Je to několik okamžitých booleanů a
počítadel s TTL:

    ContextPulse
      ac_online
      battery_percent
      explicit_session_goal
      gamemode_active
      media_playing
      desktop_locked
      input_idle_ms
      aggregate_network_rx_mib_s

### Autoritativní signály

1. explicitní asense run --goal ... -- command;
2. Feral GameMode D-Bus registration;
3. uživatelův ruční výběr v GUI/tray;
4. AC/baterie.

### Slabé pomocné signály

- GNOME idle/lock přes D-Bus;
- MPRIS pouze PlaybackStatus=Playing, bez názvu média;
- celkové network tempo;
- obecná délka input idle; žádný keycode ani evdev reader.

v0.3 vůbec neotevírá evdev kvůli klasifikaci. Explicitní Steam/GameMode
session je přesnější, levnější a nemůže se změnit v keylogger.

Malý FSM:

    explicit game/session       → session goal
    GameMode active             → Sustained
    media playing + input idle  → Quiet media
    AC + interactive load       → preference Balanced/Performance
    battery low                 → Battery/Quiet
    locked/idle bez session     → firmware Auto + úsporný profil

Přechody mají 20–30s debounce, profile cooldown a ruční override. Žádný
nekonečný heuristický engine.

Auto-profile UI má jen dvě volitelné mapy:

    AC profile       default Balanced
    Battery profile  default Quiet

Použít lze pouze aktuálně dostupný profile choice. Ruční změna pozastaví mapu
do dalšího AC↔battery přechodu; nízká baterie pod uživatelským prahem může
jednou přepnout na Quiet, nikoli přepisovat profil každé tři sekundy.

---

## 7. Steam, GameMode a typed CLI

### asense ctl

Tenký klient používá podle příkazu existující root daemon nebo lokálního
desktop ownera:

    asense ctl status [--json]
    asense ctl capabilities [--json]
    asense ctl profile performance
    asense ctl fan auto
    asense ctl fan maximum
    asense ctl fan manual --cpu 55 --gpu 60
    asense ctl pilot observe|on|off
    asense ctl pilot status
    asense ctl gpu-readiness

`status`, `capabilities`, `profile`, `fan` a `gpu-readiness` mluví přímo s
`asensed`; `pilot` mluví s desktop-owner socketem popsaným v sekci 4. Žádný
root shell a žádný raw call. Výstup používá stejné receipts jako GUI.

`asense ctl fan manual` po potvrzeném zápisu zůstane v popředí a drží
connection lease až do SIGINT/SIGTERM/EOF; potom se dnešní cestou vrátí Auto.
Maximum zachová v0.2 potvrzenou persistentní semantiku a ostatní ctl příkazy
jsou jednorázové.

### asense run

    asense run --goal sustained --profile performance -- %command%
    asense run --goal quiet -- %command%

Tok:

1. zajistit nebo aktivovat desktop ownera a předat mu pouze goal/context;
2. přes dedicated `asensed` connection načíst snapshot profilu/fan a vytvořit
   generation lease;
3. aplikovat případný explicitní profil/fan přes typed daemon;
4. spustit child process group;
5. forwardovat SIGINT/SIGTERM a přesný exit code;
6. po skončení restore jen pokud uživatel mezitím nepřevzal stav;
7. zahodit název procesu a session feature po ukončení.

Daemon i UI socket jsou `CLOEXEC` a child je nikdy nezdědí. Když wrapper zemře
včetně SIGKILL, EOF na jeho dedicated daemon connection ukončí lease a provede
stejný conditional restore. Desktop owner váže ephemeral goal/context ke
stejnému UI spojení a při EOF jej okamžitě zahodí; child tedy nemůže omylem
držet hardware session ani explicitní goal.

Steam launch option:

    asense run --goal sustained -- gamemoderun %command%

Neprovádí se process scan. V normálním běhu určuje session životnost child
process group; při abnormálním konci wrapperu ji ukončí EOF výše.

### Malé ownership receipt

Stačí generation counter, captured profile a captured fan state. Restore
proběhne jen při nezměněné generation. Tentýž mechanismus sdílí asense run,
tray auto-profile, fan commissioning a Pilot. Nevznikají čtyři rollback
implementace.

---

## 8. Tiny RWKV Thermal Forecast

### Proč RWKV

Termální data jsou 1Hz numerický proud. Rekurentní RWKV stav:

- má konstantní paměť;
- nepotřebuje text, tokenizer ani KV cache;
- běží na CPU;
- zachytí rozběh zátěže, tepelnou setrvačnost a fan lag;
- při malém rozměru stojí méně než vykreslování dashboardu.

### Reuse z voice

Matematická reference již existuje v:

    ../voice/crates/jetflow-rwkv/src/recurrent.rs
    ../voice/crates/jetflow-rwkv/src/oracle.rs

Referenční bod pro implementaci a golden parity je voice commit
`d34dd0f0f870b67cb151057824e79d82701101cc`; pozdější změna reference vyžaduje
nový ASense model schema/fixture, nikoli tiché převzetí současného `main`.

Do ASense se nepřenáší celý jetflow-rwkv, CUDA, FP64 oracle ani training stack.
Vznikne jeden fixed-shape CPU f32 modul. Voice workspace je Apache-2.0 a ASense
GPL-2.0-only, proto sdílený autor vydá extrahovaný modul také pod GPL-2.0-only,
nebo se matematika čistě reimplementuje.

### Zamčený runtime contract

    přesně 25 vstupních features + 25bit missing mask
    width 16
    2 canonical RWKV-7 bloky
    4 heads × head_dim 4
    f32 CPU inference
    RMSNorm → time-mix/generalized-delta recurrence → residual → RMSNorm →
      squared-ReLU channel-mix → residual
    recurrent state na blok: 4×4×4 matrix + time/channel shift
    input projection 50→16, output head 16→7
    přesně 8 560 vah / 34 240 B raw f32 weights
    1 Hz
    cíl pod 0,2 ms p95, release ceiling 1 ms
    žádná alokace v hot path
    60 s warm-up po startu/resume

Výstup:

    CPU delta 10/20/30 s
    GPU delta 10/20/30 s
    load-relief score 0..1

Prvních šest head hodnot je lineární ΔT. Sedmá hodnota prochází sigmoid; target
je 1, když aggregate CPU/GPU utilization+power v následujících 10 s klesne
nejméně o 20 %, 0 když zůstane do ±5 %, a mezi tím lineárně interpoluje.

Přesné pořadí 25 hodnot feature schema v1:

1. CPU temperature;
2. GPU temperature;
3. CPU 10s temperature slope;
4. GPU 10s temperature slope;
5. CPU utilization;
6. GPU utilization;
7. CPU package power;
8. GPU power;
9. battery charge/discharge power;
10. requested CPU fan percent;
11. requested GPU fan percent;
12. CPU RPM;
13. primary GPU RPM;
14. secondary/grouped GPU RPM;
15. commissioned CPU fan gain;
16. commissioned GPU fan gain;
17. current profile ordinal z live choices;
18. AC online;
19. battery percent;
20. dGPU active/sleep state;
21. maximum relevant extra-sensor temperature;
22. explicit session goal ordinal;
23. GameMode active;
24. media-playing/input-idle state;
25. normalized aggregate disk+network activity.

Jednotky a kombinace jsou pevné: temperature je °C, slope je
`(T_now - T_10s)/10` v °C/s a bez alespoň 8 z 10 vzorků je missing; power je W
a battery power je kladná při vybíjení. Utilization a requested fan jsou
procenta 0..100, commissioned gain je RPM na jeden procentní bod a booleany
AC/dGPU/GameMode mapují false/sleep=0, true/active=1. Profile mapuje `low-power=-2`,
`cool/quiet=-1`, `balanced=0`, `balanced-performance=1`, `performance=2`, jiný
token je missing. Session goal mapuje Battery=-2, Quiet=-1, Balanced=0,
Sustained=1; bez explicitního goal je missing. Media/idle je 0 interactive,
0.5 playing+idle≥60s, 1 locked nebo idle≥60s bez média. I/O je
`ln(1 + disk_MiB_s + net_MiB_s) / ln(1025)` clampnuté na 0..1.

Každá hodnota má vlastní bit v 25bit missing masce uložené v `u32`. Hodnoty
a rozbalené mask bits tvoří fixní 50hodnotovou vstupní projekci; pořadí se bez
změny feature schema nesmí měnit. Profile/fan transition se projeví v live
profile/requested-fan features a controller cooldownu; recurrent state se kvůli
vlastnímu Pilot kroku neresetuje.

Asset má little-endian header s magic, schema, topology, raw-weight length,
SHA-256 payloadu a pro každou hodnotu training mean a non-zero scale. Za
headerem následuje 8 560 little-endian f32 vah: nejdřív bias-free input
projection, potom pro každý blok přesně pole `Rwkv7LayerWeights` v pořadí
pinned voice structu a nakonec bias-free output head. Dva plné voice-compatible
RWKV-7 bloky mají 7 648 vah, bias-free input projection 50×16 má 800 a
bias-free output head 16×7 má 112. Runtime
počítá `(x - mean) / scale`, clampne na `[-5, 5]`; chybějící hodnotu po
normalizaci nastaví na nulu a zapne její mask bit. Sedm výstupů je v pevném
pořadí CPU ΔT 10/20/30 s, GPU ΔT 10/20/30 s a load-relief. Quality je vnější
deterministické skóre z warm-upu, missing features a nedávné forecast chyby,
nikoli osmý model output. Golden f64 harness obalí pinned
`Fp64Oracle::forward_frame` identickou input/output projekcí. Na 256-frame
sekvenci musí f32 runtime projít pro všech sedm výstupů i celý oracle recurrent
state s `max_abs <= 1e-4` a `max_rel <= 1e-3`.

Topologie používá voice `RwkvStackConfig::new(16, 2, 4, 4)`, RMS/group norm
epsilon `1e-5` a erase norm epsilon `1e-6`. State každého bloku je heads-major
matrix `[4][4][4]`, následovaná `time_shift[16]` a `channel_shift[16]`; oba
bloky mají celkem 192 f32 hodnot a resetují se na nulu podle pravidel níže.

### Asset, fallback a adaptace

Commitnutý model asset má SHA-256, feature schema a generující commit.
Minimální offline trainer/export je součástí 5 000 LOC rozpočtu; žádné
neomezené vedlejší ML repo. Asset vznikne ze syntetických tepelných přechodů a
sanitizovaných explicitně exportovaných commissioning traces, nikoli z
automatického logování uživatelovy aktivity.

Bez model assetu nebo při non-finite výstupu se použije lineární temperature
slope + fan lag baseline. Standardní controls fungují vždy.

- backbone je read-only;
- tři delayed RLS heads korigují 10/20/30s forecast v RAM;
- RLS začne až po warm-upu a učí se jen z pozdější skutečné teploty;
- suspend, více než 5s mezera, non-finite frame nebo změna feature schema
  resetují recurrent state i RLS;
- jednotlivé frames ani Context Pulse se neukládají;
- po restartu se residual stav zahodí;
- persistentní jsou jen fyzické commissioning agregáty fan gain/lag/thermal
  decay, bez času a workload identity.

Quality je deterministické skóre z warm-upu, missing features a nedávné chyby;
není prezentované jako kalibrovaná pravděpodobnost.

---

## 9. Thermal Pilot

Stavy:

    Off        nic nevyhodnocuje
    Observe    forecast a doporučení, žádné writes
    Active     bounded automatické fan/profile akce
    Paused     ruční zásah převzal stav
    Emergency  dnešní failsafe cesta

Default po upgrade je Observe. Uživatel zapne Active jedním přepínačem.

Goals:

- Quiet;
- Balanced;
- Sustained;
- Battery.

Každý goal má CPU/GPU ceiling, noise bias a povolení profile change.

### Controller

Pilot každou sekundu porovná jen:

    Keep
    fan +5 %
    fan +10 %
    fan -5 %
    sousední dostupný firmware profile

### Pre-ramp a no-churn

Když forecast překročí ceiling za 10–20 s:

- zvýšit fan dřív o 5–10 %;
- nepřepínat profil, pokud stačí fan;
- při predicted relief fan nezvyšovat;
- snižovat pomaleji než zvyšovat;
- minimální interval profile switch 60 s;
- ruční změna Pilot okamžitě pozastaví.

Akce používá jediný dnešní fan orchestrátor, readback a Auto/Maximum recovery.

GUI/tray drží pouze 60–120 sekund bounded RAM trace „Proč teď?“:

    CPU forecast 88 °C za 20 s
    fan 45 → 55 %
    profile zůstal Balanced

Trace zmizí po ukončení/restartu a není součástí probe.

---

## 10. Fan commissioning

Příkaz a GUI tlačítko:

    asense verify fans

Průběh:

1. odmítnout start při aktivní game session, Pilot Active, stale telemetry
   nebo teplotě nad 80 °C;
2. načíst fresh fan/profile/temperature snapshot a vytvořit generation lease;
3. krátký Auto baseline bez vytváření syntetické CPU/GPU zátěže;
4. bounded 50–60% step pro CPU a GPU, případně krátký Maximum;
5. měřit všechny RPM kanály po 1 s a při 90 °C test okamžitě ukončit;
6. klasifikovat direct/grouped/cross/no-response;
7. odhadnout lag a RPM/% gain;
8. při success, Cancel, chybě, close, socket loss i shutdown obnovit původní
   stav přes existující fan lease, pokud jej uživatel mezitím nepřevzal.

Výsledek obsahuje mapování RPM kanálů, CPU/GPU lag a gain. Tím se správně
popíše i tříventilátorový notebook, kde fan 2 a fan 3 reagují společně na GPU
command. v0.3 nevymýšlí třetí setter; dvě GPU RPM hodnoty jsou dvě
ručičky/údaje nad jedním typed GPU řízením.

Přesný UI contract pro tříventilátorový stroj:

- druhý GPU ventilátor je druhá ručička ve stávajícím GPU gauge;
- pod gauge jsou dvě samostatné GPU RPM hodnoty;
- jeden GPU manual slider řídí známou grouped dvojici;
- Fan 4+ zůstává pouze v Advanced diagnostice.

Report je bez serialu, UUID a časové historie a lze jej přiložit ke community
issue.

---

## 11. Cooling Fingerprint

Z commissioning a běžných cooldown úseků se uloží jen malé fyzické agregáty:

    fan channel mapping
    CPU/GPU lag a RPM gain
    CPU/GPU cooldown time
    RPM cooling effect
    confidence

Neukládá se průběh hry ani čas, kdy uživatel co dělal.

Po dostatečném počtu nezávislých cooldownů UI ukáže:

- odezva odpovídá vlastnímu baseline;
- cooling response je přibližně o N % slabší;
- po čištění/servisu lze vytvořit nový baseline.

Nevydává diagnózu „špatná pasta“. Je to srovnání stroje se sebou samým.

---

## 12. GPU Power Readiness

Malá diagnostika využije data, která ASense už z velké části čte:

- PCI runtime status;
- AC/baterie;
- /proc/driver/nvidia/.../power;
- nvidia-powerd.service;
- current/default/min/max/enforced power limit;
- current draw;
- throttle reasons;
- firmware profile.

Výstup:

    GPU sleeping
    Ready
    Load required
    Blocked: battery / nvidia-powerd / thermal / firmware profile
    Power limit writable: 80–140 W

Inspector nesmí probudit sleeping GPU. v0.3 pouze vysvětluje live power
envelope; obecný TGP setter zůstává mimo tento release.

---

## 13. Tray a autostart

Tray je povinná součást v0.3:

- Open ASense;
- aktuální CPU/GPU teplota bez probuzení sleeping GPU;
- aktuální profil;
- Auto / Maximum;
- Pilot Off / Observe / Active;
- Quiet / Balanced / Sustained goal;
- Exit GUI/tray.

Tray a skryté GUI jsou jeden neprivilegovaný proces. Využije se tray-icon
stack, který už přináší Dioxus desktop; nepřidává se druhý SNI framework.
asensed zůstává pouze typed hardware writer.

Lifecycle:

- CloseRequested při zapnutém tray režimu okno skryje;
- Open aktivuje existující single instance;
- autostart spustí stejný proces rovnou hidden;
- Quit skutečně ukončí tray/GUI a zavře fan/Pilot session;
- Manual nebo Pilot při hidden pokračuje, při Quit se vrátí do Auto;
- uživatelem potvrzené persistentní Maximum zachová současnou v0.2 semantiku;
- pokud systémový AppIndicator backend chybí, normální GUI funguje bez panic a
  autostart/tray volba se označí unavailable.

---

## 14. Lighting presets a notifikace

Maximálně čtyři pojmenované lighting presety:

- target;
- power;
- brightness;
- static/zones/effect;
- žádný backend packet v configu.

Při načtení se config přeloží přes aktuální LightingDevice capabilities.
Chybějící target preset zůstane unavailable.

Temperature notifications:

- volitelný CPU/GPU threshold;
- 5 °C hysteréze;
- nejvýše jedna notifikace za minutu;
- recovery až po návratu pod hysterézi;
- žádný vliv na emergency fan path.

---

## 15. GUI support report

V About/Support vznikne tlačítko:

    Create support report

Tok:

1. zavolat přímo stejnou probe::generate() cestu jako asense probe;
2. zobrazit úplný JSON náhled;
3. jasně vypsat, co report neobsahuje;
4. nabídnout Save JSON, Copy a Open prefilled GitHub issue.

Prefilled issue obsahuje model, ASense/kernel verzi a capability souhrn.
Prohlížeč neumí bezpečně automaticky přiložit lokální soubor, proto jej
uživatel přetáhne nebo vloží. Nevzniká vlastní server, token ani abuse plocha.

CLI, Preview, Copy i Save musí mít byte-for-byte stejné schema a sanitizaci;
nevznikne druhý GUI collector.

Přímý anonymní upload lze řešit až samostatně, pokud GitHub issues přestanou
stačit. Není součástí v0.3.

Probe může zahrnout sanitizovaný fan commissioning výsledek, ale nikdy
key/activity pulse, process/media data, síťové adresy, raw session trace nebo
thermal history.

---

## 16. Malé veřejné API pro vlastní GUI

Daemon zůstává GPL-2.0-only. Publikovaný typed wire contract a minimalistické
klientské příklady budou v:

    docs/CONTROL-PROTOCOL.md
    examples/asense-client/python
    examples/asense-client/rust

Požadavky:

- handshake, CAPS a typed commands;
- žádné raw WMI/HID/sysfs;
- bounded response;
- příklady status/profile/fan do několika desítek řádků a krátký samostatný
  příklad Pilot commandu přes user-owner socket;
- socket permissions stejné jako pro oficiální GUI.

Samostatné klientské ukázky mohou mít MIT licenci, aby si kdokoli napsal
vlastní GUI bez přebírání GPL aplikace. Server ani ASense core se
nepřelicencuje.

---

## 17. Persistence

Jeden atomicky nahrazovaný uživatelský state file s režimem 0600 leží pod
XDG_STATE_HOME/asense/state.json. Fyzický fingerprint je klíčovaný pouze
manufacturer/product/board/BIOS, nikdy serialem nebo UUID. Po změně BIOSu se
starý fingerprint nepoužije k řízení, ale může se zobrazit jako historický
baseline k novému commissioning testu.

Ukládá se:

- Pilot goal a Off/Observe/Active preference;
- AC/battery profile preference;
- lighting presets;
- tray/autostart preference;
- fan/cooling fyzické agregáty;
- model asset version.

Neukládá se:

- per-second telemetry;
- klávesy nebo input eventy;
- procesy/command line;
- session command/game name;
- media metadata;
- síťová historie;
- in-memory Why-now trace;
- RWKV recurrent state.

---

## 18. Protokol

v0.3 používá HELLO 3. GUI, tray, CLI a daemon se distribuují společně; nevzniká
dlouhodobá v2 kompatibilní větev. Root daemon zachová textový bounded typed
tvar:

    HELLO 3
    CAPS
    STATUS

    SESSION BEGIN ...
    SESSION END id

    VERIFY FANS START
    VERIFY FANS STATUS
    VERIFY FANS RESULT

    GPU READINESS

Neprivilegovaný desktop-owner socket má pouze `PILOT GET/SET/GOAL` a
`CONTEXT SESSION_BEGIN/SESSION_END`; nepřijímá žádný hardware packet ani root
command. Žádné JSON-RPC, raw tensors ani obecné CALL.

---

## 19. Změny po souborech

Preferovaný malý tvar:

    src/context.rs          live ephemeral desktop/session signals
    src/pilot.rs            feature frame, controller, goals
    src/thermal_rwkv.rs     fixed small CPU runtime
    src/fan_verify.rs       commissioning + fingerprint
    src/tray.rs             SNI menu and autostart glue
    src/cli.rs              ctl/run/verify dispatch
    src/support_report.rs   GUI preview/save/GitHub handoff

Existující moduly:

- telemetry.rs: extra read-only sensors a normalized frame;
- control.rs: typed v3 commands;
- daemon.rs: typed hardware/generation session lease, restore a commissioning
  runner; žádný RWKV ani Pilot decision loop;
- hardware.rs: žádný nový backend, pouze reuse;
- nvidia.rs: pouze pasivní readiness nad observer-neutral telemetry;
- app.rs: Pilot/tray/support UI wiring;
- install.sh/packaging: tray autostart a protocol docs.

Jestli samostatný soubor vytvoří víc glue než kódu, funkce zůstane v nejbližším
existujícím modulu. Nevzniká framework pro hypotetické pluginy.

---

## 20. LOC rozpočet

| Oblast | Produkce + testy |
| --- | ---: |
| Tray, autostart a quick actions | 400–500 |
| CLI, asense run, generation lease | 450–600 |
| Context Pulse + AC/GameMode/media/idle | 250–350 |
| Extra sensors a normalized features | 200–300 |
| tiny RWKV, trainer/export a parity test | 550–700 |
| Thermal Pilot controller a UI | 650–850 |
| Fan commissioning + Cooling Fingerprint | 450–600 |
| Passive GPU readiness | 150–250 |
| Support report, presets, notifikace, docs | 300–450 |
| Integrační rezerva | 0–400 |
| **Celkem** | **3 400–5 000** |

Každý patch aktualizuje jednoduchý LOC ledger. Počítá se každý commitnutý
source/test/script/doc řádek; výjimkou je pouze binární model asset. Překročení
5 000 není přijatelné schováním kódu do offline adresáře.

### Ochrana proti opakování po kompaktaci

Před implementací vzniknou dva lokální, necommitované soubory:

    .codex-v03/STATE.md
    .codex-v03/ACCEPTANCE.md

STATE drží přesný HEAD, hotový patch, aktuální LOC a jediný další krok.
ACCEPTANCE je živá kopie matice níže se stavem PASS/FAIL/NOT-RUN a konkrétním
důkazem. Po každém patchi se aktualizují dřív, než začne další práce. Neobsahují
telemetrii uživatele ani hardware identifikátory.

---

## 21. Implementační pořadí

### Patch A — Daily control surface

- asense ctl;
- asense run;
- generation lease;
- Steam/GameMode dokumentace;
- tray a autostart;
- support-report modal.

Acceptance: vlastní GUI/script může přes typed API změnit profil/fan; session
restore nepřepíše pozdější ruční změnu.

### Patch B — Telemetry truth

- extra labeled hwmon/RAPL/battery sensors;
- Context Pulse;
- observer-neutral invariant tests;
- normalized fixed feature frame;
- bounded in-memory trace.

Acceptance: sleeping dGPU zůstane sleeping; privacy-zakázaný údaj se nedostane
do configu, probe ani logu.

### Patch C — Fan commissioning

- explicitní verify runner;
- RPM mapping včetně třetího fan;
- lag/gain;
- restore přes generation lease;
- sanitizovaný report;
- základ Cooling Fingerprint.

Acceptance: PHN16-72 obnoví původní stav; fixture správně pozná grouped GPU
fan2+fan3.

### Patch D — Tiny forecast

- fixed RWKV core;
- model loader;
- reference/parity fixtures;
- slope fallback;
- Observe UI;
- corrupt/missing model fallback.

Acceptance: 1Hz runtime bez alokací a non-finite hodnot; standardní controls
fungují bez modelu.

### Patch E — Active Thermal Pilot

- goals;
- pre-ramp;
- slew/hysteresis/cooldown;
- typed action receipts;
- manual supersede/pause;
- Why-now trace;
- endurance tests.

Acceptance: žádný nový write path; Pilot sníží překmit proti statické reakci v
replay fixture a nikdy nepřepíše ruční změnu.

### Patch F — GPU/readiness a polish

- passive Dynamic Boost readiness;
- lighting presets;
- notifications;
- release/upgrade closure.

---

## 22. Acceptance matrix

| Oblast | Povinné chování |
| --- | --- |
| v0.2 regrese | PHN16-72 profiles/fans/RGB/Battery/APGE/NVIDIA beze změny |
| RTD3 | při 65s GUI+tray testu zůstane původně sleeping dGPU `suspended`, nevznikne žádná NVML session a Pilot běží CPU-only; po externím GPU workloadu se live telemetry připojí a po jeho skončení se GPU znovu uspí |
| CLI | stejné typed receipts jako GUI, přesný exit status |
| Session | SIGINT/SIGTERM, EOF/socket loss, exec failure, child exit i SIGKILL wrapperu; daemon lease vrátí hardware stav a desktop owner zahodí goal/context, pokud jej mezitím nepřevzal uživatel |
| Context | žádný process scan; pulse zmizí po TTL |
| Privacy | žádné keys/process/media/network identity v souborech ani probe |
| Auto profile | AC↔battery přechod aplikuje pouze živě dostupnou volbu; ruční profil platí do dalšího přechodu a low-battery Quiet se aplikuje nejvýše jednou |
| Sensors | vadné/absent hwmon čidlo neodstaví ostatní telemetry |
| Fan verify | direct/grouped/cross/no-response, third RPM, complete restore |
| RWKV | golden parity, corrupt asset, missing features, long finite run |
| Pilot | pre-ramp, relief, no-churn, manual pause, backend failure recovery |
| GPU readiness | pasivně vysvětlí sleep/load/blocker a nikdy nenabídne TGP write |
| Tray | single GUI instance, quick action, autostart off/on |
| Support | preview před sdílením, stable JSON, no automatic upload |
| API | příklady status/profile/fan fungují bez raw hardware přístupu |

---

## 23. Měřitelný přínos

Thermal Pilot se nevydá jako „AI“, pokud nepřinese výsledek. Replay a živé
testy porovnají firmware Auto, jednoduchou slope baseline a RWKV + Pilot.

Minimální gate:

- 20s forecast MAE alespoň o 15 % lepší než slope baseline na přechodech;
- žádné zvýšení počtu profile switchů;
- méně teplotních overshoot sekund při stejné nebo nižší špičkové hlučnosti;
- CPU overhead dlouhodobě pod 1 %;
- inference p95 pod 1 ms;
- žádné probuzení sleeping dGPU;
- žádná hardware mutace v Observe.

Pokud RWKV baseline neporazí, zůstane shadow a Pilot použije lepší jednoduchý
prediktor. Funkce se nebranduje podle modelu, ale podle výsledku.

---

## 24. Definition of done

v0.3 je hotová, když:

- v0.2 hardware coverage nemá regresi;
- tray nabízí rychlé každodenní controls;
- Steam/GameMode session funguje bez process scanneru;
- support report lze z GUI zkontrolovat a předat komunitě;
- Context Pulse se nikdy nepersistuje;
- fan commissioning popíše dva i tři RPM kanály a obnoví stav;
- tiny forecast funguje na CPU a má fallback;
- Pilot umí Observe i Active a používá jen dnešní typed backends;
- ruční změna má vždy poslední slovo;
- sleeping NVIDIA zůstane sleeping;
- GPU readiness nelže o dostupném TGP;
- Cooling Fingerprint ukládá jen fyzické agregáty;
- API dovolí vlastní jednoduché GUI bez raw/root hardware přístupu;
- celý čistý přírůstek včetně testů nepřesáhne 5 000 LOC;
- nejméně 70 % nového kódu tvoří přímá funkce nebo její test.

---

## 25. Finální rozhodnutí

ASense v0.3 nebude soutěžit počtem dashboardů, chatbotem ani seznamem
náhodných přepínačů.

Jeho náskok vznikne synergií hotových částí:

    live Acer capabilities
    + pravdivá observer-neutral telemetry
    + explicitní Steam/GameMode intent
    + fan commissioning konkrétního kusu
    + tiny recurrent forecast
    + existující fan/profile recovery
    = předvídavé, tiché a auditovatelné řízení výsledku

To je dost malé na komunitní údržbu, dost praktické na každodenní použití a
dost odlišné, aby v0.3 nebyla jen větší v0.2.
