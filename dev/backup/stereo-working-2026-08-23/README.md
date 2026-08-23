# Záloha funkční stereo konfigurace — 2026-08-23

Snímek zdrojáků z okamžiku, kdy stereo, CIS a ASE poprvé fungovaly správně.
Soubory vedle tohoto README jsou bajt za bajt kopie z `dev/core/src/`:

| soubor | původ |
| --- | --- |
| `stream.rs` | `dev/core/src/stream.rs` |
| `session.rs` | `dev/core/src/session.rs` |
| `bap.rs` | `dev/core/src/bap.rs` |
| `settings.rs` | `dev/core/src/settings.rs` |
| `agent.rs` | `dev/core/src/bin/agent.rs` |

Obnovení jednoho souboru:

```bash
cp "dev/backup/stereo-working-2026-08-23/stream.rs" "dev/core/src/stream.rs"
```

---

## Co přesně dělá stereo funkční

Tohle je ta část, kterou nesmí žádná pozdější změna rozbít. Když se sluchátka
vrátí do mono nebo do „obojí ucho hraje totéž“, chyba je skoro jistě v jednom
z těchto sedmi bodů.

### 1. Topologie: dva CIS, jeden na ucho

`StreamPlan::build` (stream.rs) volí topologii z PAC záznamu:

- `caps.supports_stereo_in_one_stream()` a `prefer_single_cis` → `Topology::SingleCis`
- jinak → `Topology::DualCis`

JBL Tune 780NC stereo v jednom streamu **nepodporuje**, takže reálná cesta je
`DualCis`. `ase_ids` jsou první dvě Sink ASE id, v pořadí, v jakém je zveřejní
zařízení:

```rust
Topology::DualCis => capabilities.sink_ase_ids.iter().take(2).copied().collect()
```

### 2. Alokace kanálů: index 0 = levá, index 1 = pravá

`StreamPlan::channel_allocation(index)`:

```rust
Topology::DualCis if (index == 0) == first_is_left => LOCATION_FRONT_LEFT,   // 0x00000001
Topology::DualCis                                  => LOCATION_FRONT_RIGHT,  // 0x00000002
```

`first_is_left = !self.swap_ears`, a `swap_ears` je natrvalo `false` — pole
zůstalo, ale nemá ovládání. Kdyby ho něco zapnulo, obě uši se prohodí neviditelně.

**Toto je bod, kde vzniká „obě uši hlásí Front Left“.** Kdyby `channel_allocation`
vracelo pro oba indexy `LOCATION_FRONT_LEFT` (což dělá `legacy` režim), zní to dutě
a v logu to nejde poznat.

### 3. Mapování ASE → CIS je poziční

`qos_and_enable_writes()`:

```rust
ascs::config_qos(ase_id, self.cig_id, index as u8, &self.qos)
//                                    ^^^^^ CIS id = pořadí ASE v plánu
```

ASE na indexu *n* dostane CIS id *n*. Stejné *n* pak `cig_command()` použije jako
`cis_id` v `LE Set CIG Parameters`. Ta rovnost je celý základ směrování — rozbití
kteréhokoliv konce prohodí kanály nebo je pošle do neexistujícího streamu.

### 4. CIG: jeden CIS na nakonfigurované ASE, sekvenční packing

`cig_command()`:

- `cis_count = ase_ids.len()` (mikrofon si přidá vlastní)
- `max_sdu_c_to_p = codec.octets_per_frame` (u DualCis, tj. **jeden** kanál na CIS,
  ne `sdu_size()`)
- `packing = PACKING_SEQUENTIAL`

Interleaved packing byl vyzkoušený a druhý kanál s ním nenajel. Sekvenční je to,
co reálně ustaví oba kanály.

### 5. Pořadí handlů se sjednocuje podle CIG, ne podle příchodu událostí

`Session::establish_once` (session.rs):

```rust
outcome.established.sort_by_key(|handle| cis_handles.iter().position(|h| h == handle)...)
```

`CIS Established` události chodí v pořadí, v jakém se kanály podaří ustavit.
Bez tohoto seřazení se levá a pravá prohodí pokaždé, když druhý kanál naskočí
první — nahodile, přibližně v polovině připojení.

### 6. Audio jde do handlu podle indexu kanálu

`AudioEncoder::stereo_packets`: levý kanál → `cis_handles[0]`, pravý → `cis_handles[1]`.
Řetěz je tedy: **levý kanál → index 0 → ASE ids[0] → CIS id 0 → cis_handles[0] → FRONT_LEFT.**

### 7. Před audiem se čeká, až jsou *obě* Sink ASE ve stavu Streaming

`agent.rs`, po `establish_isochronous`: čte se stav každé Sink ASE až 3 s a LC3 se
pošle, teprve když jsou všechny `STATE_STREAMING`. Předtím se oba streamy nakrmí
20 rámci ticha (`session.rs`, blok „Prime both channels with silence“), aby ani
jeden nebyl zařízením považovaný za nepřítomný.

Kdyby se audio poslalo s jednou ASE ještě v `Enabling`, sluchátka pustí ten jeden
kanál do obou uší — přesně příznak „pravá v obou, levá chybí“.

---

## Rychlá kontrola po změně

V logu po připojení musí být obě tyto řádky:

```
  ASE <a> → left (0x00000001)
  ASE <b> → right (0x00000002)
```

a `2 channels established`. Když je tam dvakrát `left`, stereo je rozbité,
i kdyby zvuk hrál.
