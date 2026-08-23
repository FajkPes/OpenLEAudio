# JBL TUNE 780NC - protokol (kompletni)

Zdroj: staticka analyza `jbl.stc.com` (jadx 1.5.6 + vlastni DEX parser).

## 1. Identifikace

| polozka | hodnota |
|---|---|
| Model | JBL Tune 780NC |
| **PID** | **215F** (GLOBAL) |
| Control trida | `com.harman.cmdctrl.imp.control.u3.protocol4.Tune680Control` |
| **appCmdProtocol** | **4** |
| **connectType** | **BLE** (ne SPP!) |
| bleSecurityMode | 2 |
| platform | Bes / EQ lib: airoha, chip 63E |
| eqGainRange | -6 .. +6 dB |
| eqConfig | `jbl_tune_670nc.json` |
| dash4Config | `jbl_tune_780nc.json` |
| Vzorkovaci frekvence | 44100, 48000, 88200, 96000 |

## 2. Transport - BLE GATT

```
Service        65786365-6c70-6f69-6e74-2e636f6d0000
CMD RX char    65786365-6c70-6f69-6e74-2e636f6d0001
CMD TX char    65786365-6c70-6f69-6e74-2e636f6d0002
RAW RXTX char  65786365-6C70-6F69-6E74-2E636F6D0003
CCCD           00002902-0000-1000-8000-00805f9b34fb
MTU            512 (default)
```

Base UUID je ASCII `"excelpoint.com"`. SPP (`00001101`) je v SDK take,
ale pro 780NC se nepouziva.

## 3. Ramec Protocol 4

`com.harman.protocol4.AssembleCmd4.generateCmdList()`

```
offset  velikost  pole
------  --------  -----------------------------------
  0        2      Identifier      LE  0xDD00  -> na drate: 00 DD
                                      0xDD01 = forward (sekundarni sluchatko)
  2        2      CommandID       LE
  4        1      PacketCount
  5        1      PacketIndex
  6        2      PayloadSize     LE
  8        N      Payload
```

Hlavicka 8 bajtu, vsechna 16bit pole little-endian.
Fragmentace: max payload na paket = `MTU - 15`.

### CommandID (`HeaderCommandID`)

| ID | nazev |
|---|---|
| 0x0001 | GET_DEVICE_INFO |
| 0x0002 | SET_DEVICE_INFO |
| 0x0003 | NOTIFICATION_TO_APP |
| 0x0004 | NOTIFICATION_TO_DEVICE |
| 0x00FF | RESET_TO_FACTORY |
| 0x0101 | START_OTA |
| 0x0102 | SEND_OTA_DATA |
| 0x0103 | STOP_OTA |
| 0x0104 | OTA_NOTIFICATION |
| 0x0201 | GET_DEVICE_ANALYTICS_DATA |
| 0x02FF | CLEAN_DEVICE_ANALYTICS_DATA |

### Payload

**GET (0x0001)** - jen seznam feature ID, 2 bajty LE kazde:
```
[featureId LE][featureId LE][featureId LE]...
```

**Odpoved / NOTIFICATION** - TLV, 4 bajty rezie na polozku:
```
[featureId 2B LE][valueLength 2B LE][value ...]
```

**SET (0x0002)** - `[featureId 2B LE][value ...]`
(delkove pole u SET jeste neoverene)

## 4. Feature ID mapa

Zdroj: `com.harman.sdk.message.DeviceInfoFeature4`.
`rw` = pristupova prava dle SDK.

### Zaklad / info
| ID | dec | feature | rw |
|---|---|---|---|
| 0x0001 | 1 | MTU | r |
| 0x0002 | 2 | Heartbeat | rw |
| 0x0004 | 4 | ProductID | r |
| 0x0005 | 5 | ColorID | r |
| 0x0006 | 6 | DeviceName | r |
| 0x0008/9 | 8/9 | Left/RightDeviceSerialNumber | r |
| 0x000A | 10 | MACAddress | r |
| 0x000C | 12 | FirmwareVersion | r |
| 0x000D/E | 13/14 | Left/RightDeviceBatteryStatus | r |
| 0x000F | 15 | FactoryReset | rw |
| 0x0010 | 16 | BTConnectionStatus | r |
| 0x0011 | 17 | ManualPowerOff | rw |
| 0x0012 | 18 | AutoPowerOff | rw |
| 0x0013 | 19 | AutoStandby | rw |

### ANC / ambient
| ID | dec | feature | rw |
|---|---|---|---|
| 0x0018 | 24 | **ANC** | rw |
| 0x0019 | 25 | **AA** (Ambient Aware) | rw |
| 0x001A | 26 | **TT** (TalkThru) | rw |
| 0x001B | 27 | AdaptiveANC | rw |
| 0x001C | 28 | TrueAdaptiveANC | r |
| 0x001D | 29 | LeakageCompensation | r |
| 0x001E | 30 | EarCanalCompensation | r |
| 0x001F | 31 | ANCAlwaysOn | r |
| 0x0020 | 32 | AutoCompensation | rw |
| 0x0049 | 73 | AmbientSoundControl | w |

### Zvuk / kvalita  <- "skryta nastaveni"
| ID | dec | feature | rw |
|---|---|---|---|
| 0x0015 | 21 | LRBalance | rw |
| 0x0021 | 33 | SpatialSoundMode | rw |
| 0x0022 | 34 | SpatialSoundScene | rw |
| 0x0023 | 35 | Sidetone | rw |
| 0x0024 | 36 | LowEQCompensation | rw |
| 0x0025 | 37 | **MaxVolumeLimit** | rw |
| 0x0026 | 38 | NoiseSuppression | r |
| 0x0027 | 39 | **AudioLimiter** | rw |
| 0x0028 | 40 | **HiRes** | rw |
| 0x0029 | 41 | SoundEffect | rw |
| 0x2D46 | 11590 | **AudioCodecSetting** | rw |

### EQ
| ID | dec | feature | rw |
|---|---|---|---|
| 0x0E01 | 3585 | EQInfoQuery | rw |
| 0x0E02 | 3586 | **EQInfo** | rw |
| 0x0E03 | 3587 | EQSequence (DSP realtime PEQ) | rw |
| 0x0E7E | 3710 | GetDesignEQRawData | r |
| 0x0E7F | 3711 | **EQCurve** | rw |
| 0x0E80 | 3712 | DynamicEQ | r |
| 0x0E81 | 3713 | CustomEQCount | rw |

`EQInfoFeature.EQ_UI_CUSTOM_CATEGORY = 230`

### LE Audio / Auracast
| ID | dec | feature | rw |
|---|---|---|---|
| 0x0BA0 | 2976 | **LEAudioUniCastStatus** | rw |
| 0x0B80 | 2944 | LEAudioAuraCastQuery | r |
| 0x0B81 | 2945 | LEAudioAuraCastScan | rw |
| 0x0B82 | 2946 | LEAudioAuraCastGroupInfo | r |
| 0x0B83 | 2947 | LEAudioAuraCastGroupSelect | rw |
| 0x0B84 | 2948 | LEAudioAuraCastSubgroupInfo | r |
| 0x0B85 | 2949 | LEAudioAuraCastSubgroupPlay | rw |
| 0x0B86 | 2950 | LEAudioAuraCastStatus | r |
| 0x0B87 | 2951 | LEAudioHighQualityAuraCastBroadcast | rw |
| 0x0B88 | 2952 | LEAudioAuraCasBroadcastPassword | rw |
| 0x0BA1 | 2977 | ActiveAudioSource | r |
| 0x0BA5 | 2981 | BTPairingMode | rw |

### Safe Listening (ochrana sluchu)  <- v UI 780NC CHYBI
| ID | dec | feature | rw |
|---|---|---|---|
| 0x1E80 | 7808 | SafeListeningCurrentVolume | rw |
| 0x1E81 | 7809 | **SafeListeningMaxVolumeLimit** | rw |
| 0x1E82 | 7810 | **SafeListeningDailyTimeLimit** | rw |
| 0x1E83 | 7811 | SafeListeningTodayListenedTime | rw |
| 0x1E84 | 7812 | SafeListeningPinCode | rw |
| 0x1E85 | 7813 | SafeListeningAverageVolumeHistory | rw |
| 0x1E86 | 7814 | SafeListeningDailyListenedTimeHistory | rw |
| 0x1E87 | 7815 | SafeListeningRemindVoice | rw |
| 0x1E88 | 7816 | SafeListeningRemindVoiceDelete | rw |
| 0x1E89 | 7817 | SafeListeningPrepareRecordPlayVoice | rw |
| 0x1E8A | 7818 | ...Status | rw |
| 0x1E8B | 7819 | SafeListeningTimeUpStatus | rw |

### Gesta / ovladani
Swipe 3840-3843, Tap 3844-3849, TapHold 3850-3853,
Button1/2 3970-3977, VolumeDial 3969,
LeftTouchSensitivity 4033, RightTouchSensitivity 4034.

### Stav / diagnostika
InEarStatus 4098, SealingTest/EnvironmentNoiseCheck (v0.K/L),
TWSInfo 4096, TotalPlaybackTime 4104, TotalPoweronTime 4105,
SmartSwitch 4111, PersoniFiMode 7680, HearingTestMode 7681.

### Hlasove vystupy
VoicePromptStatus 3136, VoicePromptLanguage 3137,
VoicePromptVolume 3139, FeedbackToneVolume 3140.

## 5. Smart Audio & Video (latence/kvalita)

Z `product_list.json` pro 780NC - triplety hodnot:

```
normal : [197, 46, 150]
audio  : [256, 53, 150]
video  : [197, 46, 100]
```

Vyznam poli zatim neoveren (kandidat: bitrate / interval / latence ms).
Video rezim ma treti hodnotu 100 vs 150 -> pravdepodobne latence.

## 6. UI karty pro 780NC

ANC, AA, TT, ControlCard, SmartAudioVideoCard, AuracastCard,
LeAudioCard, VoicePromptCard, EQCard, Spatial3DCard, PersoniFiCard,
LeftRightBalanceCard, VoiceAwareCard, RelaxSoundCard, AutoPowerOffCard,
SupportCard, SoftwareCard, ResetToFactoryRest, OTA.

`isSupportConfig`: ANC, DashBoardPowerOff, Hotword, SnNumber.

**Zadna SafeListening karta** - proto v appce chybi ochrana sluchu.

## 7. Zbyva overit na zarizeni

1. Odpovida firmware 780NC na SafeListening feature (7808-7819)?
2. Presny format `value` u EQInfo / EQCurve
3. Delkove pole u SET payloadu
4. Vyznam trojic SmartAudioVideo
5. Autentizace: `AuthenticateDevice` (62), `AdvancedAuthenticateDevice` (75),
   `AuthenticationAlgorithm` (74) - zda je nutna pred zapisem

## 8. Potvrzeno ze snimku oficialni appky

Snimky JBL Headphones app pro 780NC (firmware v dobe porizeni).

### General
| karta v appce | feature |
|---|---|
| Ambient Sound Control — hlavni prepinac | `AmbientSoundControl` 73 (w) |
| Noise Cancelling / Ambient Aware / TalkThru | `ANC` 24 / `AA` 25 / `TT` 26 |
| Control | gesta 3840-3853, tlacitka 3970-3977 |
| Smart Audio & Video — Audio Mode / Video Mode | `smartAudioVideo` triplety |
| Auracast (Disabled) | 2944-2952 |
| **LE Audio — prepinac** | `LEAudioUniCastStatus` **2976** |
| Voice Prompts (English) | `VoicePromptStatus` 3136 / `VoicePromptLanguage` 3137 |

### Audio
| karta | feature |
|---|---|
| **Equalizer — 10 pasem** | `EQInfo` 3586 / `EQCurve` 3711 |
| Spatial Sound — Movie / Music / Game | `SpatialSoundMode` 33 / `SpatialSoundScene` 34 |
| Personi-Fi | `PersoniFiMode` 7680, `HearingTestMode` 7681 |
| Left / Right Sound Balance | `LRBalance` 21 |
| Voice Aware | `Sidetone` 35 (kandidat) |

### Other
| karta | feature |
|---|---|
| Relax Mode — 5 zvuku + casovac | `SleepSetting` 3249 / `AlarmRingTone` 3252 (kandidat) |
| Auto Power Off — 30 min / 1 hr / 2 hr | `AutoPowerOff` **18** (StatusLevelSize2) |
| Support | - |

### EQ - potvrzene parametry

```
pasma:  32  64  125  250  500  1k  2k  4k  8k  16k   (10 pasem)
rozsah: -6 .. +6 dB   (z product_list.json)
```

Profily jsou pojmenovane a cislovane (`JBL Tune 780NC EQ 62`, `V50`, `V60`),
tedy uzivatelske sloty. `CustomEQCount` 3713 drzi jejich pocet,
`EQ_UI_CUSTOM_CATEGORY = 230`.

### Potvrzeno jako CHYBEJICI v oficialni appce

Zadna karta pro:
`SafeListening*` (7808-7819), `MaxVolumeLimit` 37, `AudioLimiter` 39,
`HiRes` 40, `AudioCodecSetting` 11590.

**Dusledek:** cokoliv z tohohle zapiseme, oficialni appka neuvidi ani
nevrati. Zpet jen nasim SET nebo tovarnim resetem. Proto jsou tyhle
featury v `app/risk.js` oznacene jako `high` a v UI skryte.

---

# JBL TUNE 720BT — Protocol 3

Uplne jina generace protokolu nez 780NC. Stejny transport (BLE),
jine ramcovani a jina logika prikazu.

| polozka | 780NC | 720BT |
|---|---|---|
| PID | 215F | **20B4** |
| uiVersion | 4.0 | **3.1** |
| Control trida | `protocol4.Tune680Control` | **`bes.Tune720BTControl`** |
| Protokol | Protocol 4 (feature ID) | **Protocol 3 (cmd ID)** |
| Ramec | `00 DD` + 8B hlavicka | **`AA` + cmdId + delka** |
| Transport | BLE | BLE (stejne UUID) |

## Ramec

```
AA <cmdId> <len:1B> <payload...>          CmdBase.combine(h, p)
AA <cmdId> 00                             CmdBase.combine(h)      - bez payloadu
AA <cmdId> <lenLo> <lenHi> <payload...>   CmdBase.combineLen2     - SafeListening
```

Pozor: `combine(h)` posila **delku 0 a zadny payload**, zatimco
`combineSub(h)` posila **delku 1 a payload `00`**. Neni to totez.

## Prikazy (CommandHeader, druhy bajt za 0xAA)

| cmd | nazev | cmd | nazev |
|---|---|---|---|
| 0x01 | SET_APP_ACK | 0x71 | SET_GESTURE_CONTROL |
| 0x03 | SET_APP_BYE | 0x72 | REQ_GESTURE_CONTROL |
| 0x05 | SET_APP_FIN_ACK | 0x73 | RET_CUSTOMIZE_TOUCH |
| 0x11 | REQ_DEV_INFO | 0x74 | SET_ANC_TUNING |
| 0x13 | REQ_ANALYTICS | 0x75 | REQ_ANC_TUNING |
| 0x15 | CLEAN_ANALYTICS | 0x77 | GESTURE_IN_BATCH |
| 0x21 | REQ_DEV_STATUS | 0x78 | FUNCTION_CONTROL |
| 0x23 | REQ_EAR_BUDS_BEEPING | 0x79 | TOUCH_PANEL_CONTROL |
| 0x25 | REQ_BATTERY_INFO | 0x81 | SET_SMART_SWITCH |
| 0x26 | CASE_VERSION | 0x82 | REQ_SMART_SWITCH |
| 0x31 | SET_ANC | 0x90 | SLEEP_BUD / SILENT_NOW |
| 0x32 | SET_AA_MODE | 0x91 | ANC_MODES |
| 0x33 | SET_AUTO_OFF | 0x92 | HOTWORD_MODE |
| 0x34 | SET_MULTI_AI | 0x93 | MULTI_LANG_VOICE_PROMPT |
| 0x35 | SET_AUTO_PLAY_PAUSE | 0x94 | SERIES_NUMBER |
| 0x36 | SET_EAR_BEEPING | 0x95 | FACTORY_RESET |
| 0x40 | SET_EQ_PRESET | 0x96 | STANDBY_MODE |
| 0x41 | SET_CUSTOM_EQ | 0x97 | SHUT_DOWN |
| 0x42 | REQ_CUSTOM_EQ | 0x98 | VOICE_AWARE |
| 0x44/45 | PERSONIFY_EN + DJ_IDX | 0x9B | REQ_DEVICE_MULTI_STATUS |
| 0x47/49 | PERSONIFI_MODE + HEARING | 0xA8 | LEFT_RIGHT_BALANCE |
| 0x4A | PERSONIFI_VOLUME | 0xA9 | CASE_NOTIFICATION |
| 0x51 | **SAFE_LISTENING** | 0xF0 | DFU / OTA |
| 0x61 | REQ_SYNC_APP_STATUS | | |

## Overene payloady

```
AA 11 00              informace o zarizeni
AA 25 01 00           baterie
AA 94 01 01           seriove cislo
AA 9B 02 01 01        souhrnny stav
AA 91 01 11           ANC rezimy - precist
AA 91 01 01           ANC podrezimy - precist
AA 91 01 13           vypnout ANC i Ambient
AA 91 <n> 10 <mode> <val> ...   nastavit ANC rezimy
AA 31 01 <0|1>        ANC zap/vyp
AA 32 01 <idx>        Ambient Aware rezim
AA 40 01 <idx>        EQ preset
AA 42 01 <idx>        vlastni EQ - precist
AA 98 01 01           Voice Aware - precist
AA 98 02 00 <lvl>     Voice Aware - uroven
AA 78 01 01           function control - precist
AA 78 03 00 01 <val>  function control - nastavit
AA A8 01 01           L/R balance - precist
AA A8 05 00 01 <on> 02 <lvl>    L/R balance - nastavit
AA 33 01 <bit7=zap|minuty>      auto vypnuti
AA 93 01 01           hlasove hlasky - precist
AA 93 02 03 <0|1>     hlasove hlasky zap/vyp
AA 97 00              vypnout sluchatka
AA 95 00              tovarni reset
```

## Safe Sound (0x51) — 720BT to MA i v oficialni appce

Dvoubajtova delka. GET: `AA 51 <lenLo> <lenHi> 01 <featureId...>`

| id | feature | rw |
|---|---|---|
| 1 | MaximumVolumeLimit | RW |
| 2 | CurrentVolume | RW |
| 3 | DailyTimeLimitSwitch | RW |
| 4 | DailyTimeLimit | RW |
| 5 | TodayListenedTime | R |
| 6 | PinCode | RW |
| 7 | AverageVolumeHistory | R |
| 8 | DailyListenedTimeHistory | R |
| 9 | RemindVoiceType | R |
| 10 | PrepareRecordYourVoice | R |

Precteni vsech najednou:
`AA 51 0B 00 01 01 02 03 04 05 06 07 08 09 0A`

## UI karty 720BT (dash3)

ANC, AA, TT, EQCard, VoiceAwareCard, SmartAudioVideoCard,
LeftRightBalanceCard, VoicePromptCard, **SafeSoundCard**,
AutoPowerOffCard, SupportCard, SoftwareCard, ResetToFactory, OTA.

`isSupportConfig`: DashBoardPowerOff, SnNumber.
