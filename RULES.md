# Regelsatz alert-bot

Eine konsistente Spezifikation aller Eingabeformate. Diese Datei ist die
Quelle der Wahrheit — wenn die Implementierung abweicht, ist die
Implementierung falsch. Bekannte Abweichungen stehen in der lokalen, nicht versionierten `BACKLOG.md`;
diese Spezifikation ist keine Behauptung vollständiger Implementierung.

This German-language document specifies the intended input semantics. Known
implementation gaps are tracked locally in the unversioned `BACKLOG.md`.

## Zwei Default-Regeln, immer

Jede Eingabe besteht aus einem **Zeit-Ausdruck** gefolgt vom **Reminder-Text**.
Wenn keine Uhrzeit angegeben ist, gilt einer von zwei Defaults — je nachdem
wie der Zeit-Ausdruck gemeint ist:

| Art des Zeit-Ausdrucks | Default-Uhrzeit | Beispiel |
|---|---|---|
| **Relativ** — eine Zeitspanne ab jetzt (`5m`, `22d`, `1Y`, `7h30m`, `*2d`) | exakt die Uhrzeit zu der der Befehl gesendet wurde | `/alert 22d Geburtstag` um 14:32 → in 22 Tagen um 14:32 |
| **Absolut** — ein Datum oder benannter Tag (`30.4.26`, `morgen`, `do`, `*1.`, `*24.12`) | 09:00 | `/alert morgen Arzt` → morgen 09:00 |

Beide lassen sich mit einer expliziten Uhrzeit überschreiben:
`/alert morgen 14:30 Arzt` → morgen 14:30.
`/alert *2d 11:00 Vitamin` → alle 2 Tage um 11:00.

Sonderfall **bare clock-time**: `/alert 22:00 text` ist Kurzform für
„nächstes Mal 22:00". Wenn heute 22:00 noch nicht durch ist → heute, sonst →
morgen. Gleiche Logik wie bei Wochentagen — keine Sonderregeln, nur
„nächstes Auftreten".

## Suffixe für relative Zeitspannen

Case-sensitiv: `m` ≠ `M`.

| Suffix | Bedeutung | Auflösung |
|---|---|---|
| `s` | Sekunden | exakt |
| `m` | Minuten | exakt |
| `h` | Stunden | exakt |
| `d` | Tage | exakt 24 × 3600 s |
| `w` | Wochen | exakt 7 × 24 h (Convenience für `7d`) |
| `M` | Monate | kalender-aware: selber Monatstag X Monate später |
| `Y` | Jahre | kalender-aware: selbes Datum X Jahre später |

Plus Langformen für Lesbarkeit (case-insensitiv): `min`/`minute`/`minuten`,
`std`/`stunde`/`stunden`, `tag`/`tage`/`day`/`days`, `woche`/`wochen`/`week`/`weeks`,
`monat`/`monate`/`month`/`months`, `jahr`/`jahre`/`year`/`years`.

## Erlaubte Uhrzeit-Formate

Werden vom Parser an jeder Stelle akzeptiert wo eine Uhrzeit erwartet wird
(Override bei Datums-/Named-Day-Specs, Override bei Recurring, oder
bare-clock-time direkt nach `/alert`).

| Format | Beispiel | Auflösung |
|---|---|---|
| 24h mit Doppelpunkt | `14:30` | 14:30 |
| 24h nur Stunde | `14` | 14:00 |
| 24h mit „Uhr" | `9 Uhr`, `14:30 Uhr` | wie oben, „Uhr" optional |
| 12h am/pm | `9am`, `2pm`, `2:30pm` | 12h → 24h |
| 12h Sonderfälle | `12am`, `12pm` | 00:00, 12:00 |
| optional „um"/„at" davor | `um 14:30`, `at 9am` | gleich, Konnektor optional |

**Nicht akzeptiert**: `22h` als Uhrzeit (kollidiert mit `22h` als
„22 Stunden"-Offset). Wenn Uhr-Notation gewünscht, dann `22 Uhr` oder `22:00`.

## Erlaubte Datums-Formate

Tag und Monat dürfen einstellig oder zweistellig sein, Jahr zweistellig
oder vierstellig. Alle Kombinationen sind gültig.

| Format | Beispiel | Auflösung |
|---|---|---|
| TT.MM.JJ | `30.4.26`, `30.04.26`, `1.5.27` | 2-stelliges Jahr → 20JJ |
| TT.MM.JJJJ | `30.04.2026`, `30.4.2026`, `1.5.2027` | wie eingegeben |
| TT.MM (ohne Jahr) | `30.4`, `30.04` | dieses Jahr wenn noch zukünftig, sonst nächstes |
| TT.MM. (Trailing-Dot) | `30.4.`, `30.04.` | wie ohne Trailing-Dot |
| ISO JJJJ-MM-TT | `2026-04-30`, `2026-4-30` | für Copy-Paste aus Logs/APIs |

## Wochentage und benannte Tage

Die deutschen Standard-Schreibweisen und ihre üblichen Kürzel, plus die
englischen Äquivalente. Levenshtein-Fuzzy-Matching fängt Tippfehler
(z. B. `donnerstah` → Donnerstag) ab.

**`heute`/`today` sind absichtlich NICHT akzeptiert.** Für einen Reminder
heute schreibst du direkt die Uhrzeit: `/alert 22:00 Bier`. Wenn du
trotzdem `heute` tippst, kommt ein Hinweis mit dieser Korrektur.

| Bedeutung | DE | EN |
|---|---|---|
| Morgen (+1 Tag) | `morgen` | `tomorrow` |
| Übermorgen (+2 Tage) | `übermorgen`, `uebermorgen` | — |
| Montag | `montag`, `mo`, `mon` | `monday`, `mon` |
| Dienstag | `dienstag`, `di` | `tuesday`, `tue`, `tues` |
| Mittwoch | `mittwoch`, `mi` | `wednesday`, `wed` |
| Donnerstag | `donnerstag`, `do` | `thursday`, `thu`, `thurs` |
| Freitag | `freitag`, `fr` | `friday`, `fri` |
| Samstag | `samstag`, `sa` | `saturday`, `sat` |
| Sonntag | `sonntag`, `so` | `sunday`, `sun` |

Großschreibung egal: `Montag` = `montag` = `MONTAG`.

Mehrfach-Wochentage werden komma-getrennt: `mo,mi,fr` oder `Mo,Mi,Fr`.

### Auflösung bei Wochentag-Spezifikation (one-shot)

Regel: **erstes zukünftiges Auftreten gewinnt.** Wenn heute der Zieltag ist
und die Uhrzeit noch nicht passiert ist → heute. Sonst → nächste Woche.

| Heute | Befehl | Auflösung |
|---|---|---|
| Sa 17:00 | `/alert sa 20:00 Bier` | heute 20:00 |
| Sa 21:00 | `/alert sa 20:00 Bier` | nächste Woche Sa 20:00 |
| Sa 8:00 | `/alert sa Putzen` (default 09:00) | heute 09:00 |
| Sa 10:00 | `/alert sa Putzen` (default 09:00) | nächste Woche Sa 09:00 |
| Mi 12:00 | `/alert do 14:00 Standup` | morgen (Do) 14:00 |

## Basisbefehle (einmalig)

### Relativ einfach

| Befehl | Was passiert | Was **nicht** passieren soll |
|---|---|---|
| `/alert 10s text` | in 10 Sekunden | — |
| `/alert 5m text` | in 5 Minuten | — |
| `/alert 7h text` | in 7 Stunden | — |
| `/alert 22d text` | in 22 Tagen, zur exakt gleichen Uhrzeit | generische Default-Zeit wie 09:00 |
| `/alert 3w text` | in 3 Wochen (= 21 Tagen) | — |
| `/alert 2M text` | am selben Monatstag in 2 Monaten, gleiche Uhrzeit | 60 Tage rechnen |
| `/alert 1Y text` | selbes Datum nächstes Jahr, gleiche Uhrzeit | 365 Tage rechnen |

### Relativ mit Uhrzeit-Override

Nur erlaubt wenn der relative Spec keine sub-day-Einheiten enthält
(also keine `s`/`m`/`h`). Dann ist „Datum von jetzt + Offset, Uhrzeit aus
Override" eindeutig.

| Befehl | Was passiert |
|---|---|
| `/alert 2d 11:00 text` | übermorgen 11:00 |
| `/alert 1w 18:00 text` | in 7 Tagen 18:00 |
| `/alert 1Y 9:00 text` | nächstes Jahr selbes Datum 09:00 |
| `/alert 15h 11:00 text` | **Fehler** — sub-day + Override ist sinnlos |
| `/alert 2d8h 11:00 text` | **Fehler** — Mischung von d und h + Override |

### Relativ kombiniert (ohne Override)

Komponenten in **absteigender** Reihenfolge `Y → M → w → d → h → m → s`.
Keine Leerzeichen zwischen Komponenten.

| Befehl | Was passiert |
|---|---|
| `/alert 7h30m text` | in 7 Stunden 30 Minuten |
| `/alert 2d12h text` | in 2 Tagen 12 Stunden (jetzt-Uhrzeit + 60h) |
| `/alert 1Y2M15d8h40m20s text` | maximaler Case, sinnlos aber funktioniert |

### Absolut Datum

| Befehl | Was passiert | Default |
|---|---|---|
| `/alert 30.4.26 text` | am 30.04.2026 | 09:00 |
| `/alert 30.4.26 14:30 text` | am 30.04.2026 um 14:30 | — |
| `/alert 30.4 text` | nächstes Auftreten von 30.4. | 09:00 |
| `/alert 30.04.2026 14 Uhr text` | am 30.04.2026 um 14:00 | — |
| `/alert 2026-04-30 text` | ISO-Form: am 30.04.2026 | 09:00 |

### Bare Clock-Time (nächstes Auftreten)

| Befehl | Heute noch nicht vorbei → | Heute schon vorbei → |
|---|---|---|
| `/alert 22:00 text` | heute 22:00 | morgen 22:00 |
| `/alert 14 Uhr text` | heute 14:00 | morgen 14:00 |
| `/alert 9am text` | heute 09:00 | morgen 09:00 |

### Named Day

| Befehl | Was passiert | Default |
|---|---|---|
| `/alert morgen text` | morgen | 09:00 |
| `/alert morgen 14 Uhr text` | morgen 14:00 | — |
| `/alert übermorgen text` | in 2 Tagen | 09:00 |
| `/alert do text` | nächster Donnerstag (heute wenn 09:00 noch in Zukunft) | 09:00 |
| `/alert sa 20:00 text` | nächster Samstag um 20:00 (heute wenn 20:00 noch nicht durch) | — |

## Wiederholungen

Marker: `*` direkt vorm Zeit-Ausdruck, oder Triggerwort `every`/`alle`/`jeden`/`jede`.
Beide äquivalent. Default-Regeln für Uhrzeit gelten genauso wie bei
einmaligen Befehlen.

### Relativ

| Befehl | Was passiert |
|---|---|
| `/alert *30m text` | alle 30 Minuten, erste Feuerung in 30 Minuten |
| `/alert *3h text` | alle 3 Stunden, erste Feuerung in 3 Stunden |
| `/alert *2d text` | alle 2 Tage zur Erstellungs-Uhrzeit, erste Feuerung in 2 Tagen |
| `/alert *2d 11:00 text` | alle 2 Tage um 11:00 (override) |
| `/alert *3M text` | jeden Monat + 3, gleicher Monatstag, gleiche Uhrzeit |
| `/alert *3M 18:00 text` | gleiche Monatstag-Logik, aber um 18:00 |
| `/alert *1Y text` | jährlich, gleiches Datum, gleiche Uhrzeit |
| `/alert alle 2d 11:00 text` | identisch zu `*2d 11:00 text` |
| `/alert jeden 7d 9:00 text` | identisch zu `*7d 9:00 text` |

**Erste Feuerung.** Bei jedem `*N<Einheit>` liegt die erste Feuerung ein volles
Intervall in der Zukunft — `*6d` also in 6 Tagen, nicht morgen. Ausnahme: mit
Uhrzeit-Override, dessen Uhrzeit heute (bzw. in diesem Monat/Jahr) noch
bevorsteht, wird dieser nächstgelegene Slot genommen — `*1d 22:00` um 20:00
feuert also noch heute Abend.

### Wochentage

| Befehl | Was passiert |
|---|---|
| `/alert *do text` | jeden Donnerstag 09:00 |
| `/alert *do 14:00 text` | jeden Donnerstag 14:00 |
| `/alert *mo,mi,fr text` | jeden Mo/Mi/Fr 09:00 |
| `/alert *mo,mi,fr 9 text` | jeden Mo/Mi/Fr 09:00 (explizit) |
| `/alert jeden montag,mittwoch,freitag 9 Uhr text` | identisch |

### Tag im Monat

| Befehl | Was passiert |
|---|---|
| `/alert *1. text` | jeden 1. eines Monats, 09:00 |
| `/alert *15. 18:00 text` | jeden 15. eines Monats, 18:00 |
| `/alert *31. text` | jeden 31. — in Monaten ohne 31. wird der letzte Tag genommen (28./29./30.). Bei Erstellung kommt ein einmaliger Hinweis |

### Datum im Jahr

| Befehl | Was passiert |
|---|---|
| `/alert *24.12 text` | jeden 24.12., 09:00 |
| `/alert *24.12 18:00 text` | jeden 24.12., 18:00 |
| `/alert *29.2 text` | jeden 29.2. — in Nicht-Schaltjahren wird der 28.2. genommen. Bei Erstellung kommt ein einmaliger Hinweis |

## Globale Regeln

- Alles **nach** dem Zeit-Ausdruck (inkl. optionaler Uhrzeit-Override) ist
  Reminder-Text, wortwörtlich. Keine weitere Magie.
- Kombinationen müssen in absteigender Größenordnung stehen: `Y > M > w > d > h > m > s`.
- Wenn der aufgelöste Feuer-Zeitpunkt in der Vergangenheit liegt, kommt ein
  Fehler („Zeitpunkt liegt in der Vergangenheit"). Ausnahme: Wochentags-Namen
  und Datum-ohne-Jahr suchen automatisch das nächste zukünftige Auftreten.
- Wiederholungen brauchen **minimum 30 Minuten** Interval. Kleinere Werte
  werden mit Hinweis abgelehnt.
- Relative One-shots sind auf maximal **50 Jahre** ab jetzt gekappt, mit
  Hinweis bei Überschreitung. Wiederholungen haben keine festgelegte Endanzahl;
  die Intervallgröße muss jedoch in die verwendete RRULE-Einheit passen
  (maximal 65535) und der Zeitpunkt darstellbar sein. Größere Werte werden abgelehnt.
- Tippfehler in Wochentag-/Monatsnamen werden fuzzy korrigiert
  (Levenshtein-Distanz adaptiv: ≤ 3 Zeichen exakt, 4-5 Zeichen Distanz ≤ 1,
  ab 6 Zeichen Distanz ≤ 2). Reminder-Text wird **nie** fuzzy korrigiert.

## Edge Cases

| Fall | Verhalten |
|---|---|
| 31. Januar + 1M | 28./29. Februar (letzter Tag des Folgemonats), einmaliger Hinweis bei Erstellung |
| 29. Februar 2024 + 1Y | 28. Februar 2025 (Schaltjahr-Korrektur nach unten), einmaliger Hinweis |
| `*1M`/`*3M` am 29., 30. oder 31. erstellt | feuert in kürzeren Monaten am letzten Tag des Monats, einmaliger Hinweis bei Erstellung |
| `*1Y` am 29. Februar erstellt | feuert in Nicht-Schaltjahren am 28.2., einmaliger Hinweis bei Erstellung |
| `*29.2` recurring im Nicht-Schaltjahr | feuert am 28.2. (Letzter-Tag-Logik), einmaliger Hinweis bei Erstellung |
| `*31.` recurring in Februar/April/Juni/September/November | feuert am letzten Tag des Monats, einmaliger Hinweis bei Erstellung |
| Uhrzeit fällt in DST-Lücke (z. B. 02:30 am Frühlings-Sonntag) | bumpt eine Stunde vor |
| Uhrzeit ist DST-doppeldeutig (Herbst-Sonntag 02:30) | nimmt die frühere Instanz |
| `*<intervall>` aber Bot war offline | bei Wiederanlauf wird die nächste zukünftige Feuerung berechnet, vergangene werden übersprungen, dafür gibt's eine Catch-Up-Notiz vor dem Reminder |
| Wochentag heute, gewünschte Uhrzeit schon vorbei | nächste Woche |
| Wochentag heute, gewünschte Uhrzeit noch nicht durch | heute |
| Bare clock-time bereits vorbei (`/alert 14:00` um 15:00) | morgen 14:00 (nächstes Auftreten) |
| `/alert heute ...` oder `/alert today ...` | Fehler mit Hinweis: „nimm direkt die Uhrzeit, z. B. /alert 22:00 text" |

## Beispiele die **nicht** funktionieren sollen

| Eingabe | Warum nicht |
|---|---|
| `/alert 15h 11:00 text` | sub-day-Relativ + Override ist sinnlos |
| `/alert 2d8h 11:00 text` | Mischung aus day und sub-day + Override |
| `/alert 30m2h text` | falsche Reihenfolge (`h` muss vor `m` kommen) |
| `/alert *15m text` | unter Minimum-Interval (30m) |
| `/alert 100Y text` | über Maximum (50 Jahre) |
| `/alert text 5m` | Zeit-Ausdruck muss zuerst kommen |
| `/alert 5x text` | `x` ist kein gültiger Suffix |
| `/alert heute 22:00 text` | „heute" ist kein Keyword, Hinweis |
| `/alert 22h text` | `22h` ist Offset (= 22 Stunden), nicht Uhrzeit |
