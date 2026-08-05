# RAW test fixtures — what each file is for

The RAWs themselves are **git-ignored** (large, and some carry a licence that excludes
redistribution — see `.gitignore`). This file is the tracked index so visual test plans
can cite fixtures by name and a fresh clone knows what to re-acquire.

Acquired 2026-08-05 from **raw.pixls.us** (CC0) and **signatureedits.com** (explicit
free-use grant; *not* CC0 — do not redistribute those). Full provenance, source URLs and
licence text live in the acquisition manifest at
`~/Downloads/ferrolite-raw-fixtures/MANIFEST.md`.

**All values below are what `rawler` reports** — i.e. what FerroLite actually shows — not
what exiftool sees. The two disagree in places; that is called out where it matters.

## The set

| File | Camera | rawler lens | ISO | Focal | Dims | Use it for |
|---|---|---|---|---|---|---|
| `iso0100-panasonic-dc-s5-iso100-85mm.RW2` | Panasonic DC-S5 | *(none)* | 100 | 85mm | 6048×4016 | NR baseline / clean reference |
| `iso0200-panasonic-dc-s5-iso200-105mm.RW2` | Panasonic DC-S5 | *(none)* | 200 | 105mm | 6072×4016 | NR ladder; **Rotate270** |
| `iso0640-panasonic-dc-s5-iso640-60mm.RW2` | Panasonic DC-S5 | *(none)* | 640 | 60mm | 6072×4016 | NR ladder |
| `iso4000-panasonic-dc-s5-iso4000-20mm.RW2` | Panasonic DC-S5 | *(none)* | 4000 | 20mm | 6072×4016 | **Real shadow noise** — night sky. Best NR subject. |
| `iso6400-panasonic-dc-s5-iso6400-85mm.RW2` | Panasonic DC-S5 | *(none)* | 6400 | 85mm | 6048×4016 | **Top ISO.** NR strength + `NR_STRENGTH_SCALE` tuning |
| `xtrans-fujifilm-x-t3-iso160-35mm.RAF` | Fujifilm X-T3 | XF35mmF2 R WR | 160 | 35mm | 6384×4182 | **Non-Bayer X-Trans** — demosaic path |
| `bayer-bggr-pentax-k7-iso3200-135mm.PEF` | Pentax K-7 | smc PENTAX-DA 18-135 | 3200 | 135mm | 4736×3136 | **BGGR** (non-RGGB) + PEF + high ISO on APS-C |
| `highres61mp-sony-ilce7rm5-iso100-50mm.ARW` | Sony A7R V | *(none)* | 100 | 50mm | 9728×6656 | **Largest file (86 MB).** Memory + perf gates |
| `cr3-canon-eosr5-iso100-50mm.CR3` | Canon EOS R5 | 50mm f/1.4 DG HSM \| A | 100 | 50mm | 8352×5586 | CR3 container; second high-MP body |
| `ultrawide-sony-ilce7m3-iso125-16mm.ARW` | Sony A7 III | FE 16-35mm F2.8 GM | 125 | **16mm** | 6048×4024 | Lens distortion; Lensfun **match** path; **Rotate270** |
| `keystone-sony-ilce7m4-iso320-25mm.ARW` | Sony A7 IV | FE 16-35mm F2.8 GM | 320 | 25mm | 7040×4688 | **Converging verticals** — perspective/upright; **Rotate270** |
| `haze-sony-ilce7m4-iso320-50mm.ARW` | Sony A7 IV | *(none)* | 320 | 50mm | 7168×5120 | **Dense fog/haze** — dehaze; **Rotate270** |
| `skysubject-sony-ilce7m4-iso800-24mm.ARW` | Sony A7 IV | FE 16-35mm F2.8 GM | 800 | 24mm | 7040×4688 | **Clean subject edge vs sky** — masking; **Rotate270** |
| `tele-sony-ilce7m3-iso100-172mm.ARW` | Sony A7 III | 70-180mm F2.8 Di III VXD | 100 | **172mm** | 6048×4024 | Long tele; mild distance haze; **Rotate270** |
| `rotated-nikon-df-iso100-35mm.NEF` | Nikon Df | *(none)* | 100 | 35mm | 4992×3292 | **Rotate90** on landscape-stored pixels; Lensfun **no-match** path |
| `mftnoise-panasonic-dc-g9-iso3200-25mm.RW2` | Panasonic DC-G9 | G 25mm F1.7 Asph. | 3200 | 25mm | 5264×3912 | ISO 3200 on **MFT** — noisier than FF at same ISO |
| `sample.rw2` | Panasonic DMC-LX3 | *(none)* | 100 | 12.8mm | 4060×2250 | **Committed** fixture the decode tests depend on |
| `DSC04692.ARW` / `DSC04693.ARW` | Sony A7 II | FE 28-70mm | 100 | 59 / 28mm | 6048×4024 | The author's own originals |

`../raw-broken/` holds two files rawler cannot handle — see its README. It sits
outside this directory on purpose: `Catalog::ingest_folder` recurses, so a
subdirectory here would be swept into every ingest test.

## Known caveats

**The ISO ladder is not a controlled series.** 100 / 200 / 640 / 4000 / 6400 are five
*different scenes* on three different lenses. It answers "does NR cope at high ISO"; it
does **not** answer "does one slider value behave consistently as ISO rises". A real
single-scene tripod ladder still needs shooting.

**No ISO 12800.** Neither source had one for this body.

**rawler reports no lens for 7 of these files** even though exiftool does — all five
Panasonic S5 frames, plus the A7R V and the A7 IV haze shot. So the develop panel shows
no lens and Lensfun cannot match on them. That is an upstream rawler gap, not a fixture
defect; treat the "no lens" files as free coverage of the unmatched-lens path.

**Six files carry `Rotate270`** and one `Rotate90`. Handy for orientation testing, but
be aware most of the Signature Edits set is affected — if you want a plain landscape
frame, use a Panasonic S5 file or the author's own ARWs.
