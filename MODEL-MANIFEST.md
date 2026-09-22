# Model manifest integrity

`models.json` identifies a model primarily by its canonical file name and, when provided, a SHA-2 digest.

Recommended entry:

```json
{
  "name": "example.onnx",
  "model_type": "Vision",
  "used_for": "Example task",
  "supports": ["cuda", "directml", "cpu"],
  "author": "Example",
  "sha256": "<64 lowercase hexadecimal characters>",
  "sha512": ""
}
```

Rules:

- File size is presentation metadata only and is never used to identify or validate a model.
- `sha512` has priority when both SHA-512 and SHA-256 are present.
- `sha256` is the recommended default for a published model manifest.
- If neither SHA-2 digest is published, the application requires the exact manifest file name and records a local SHA-256 fingerprint. This detects later changes but is not a substitute for a publisher-provided digest.
- Legacy MD5/size registry entries are ignored and migrated automatically after the next successful SHA-2 reconciliation.
- A selected file whose name differs from the manifest can still be recognized if its published SHA-256 or SHA-512 digest matches a manifest entry.

On Windows, the per-model cache is stored under:

`HKCU\\Software\\Gallery Inc\\AIModelManager\\Models`

The cache stores the canonical file name/path, SHA-2 algorithm/value, last-write token and verification status. It does not use file size as part of identity.

## Published digests in 1.0.2

| File | SHA-256 | SHA-512 |
|---|---|---|
| `birefnetv2-img.onnx` | `58F621F00F5D756097615970A88A791584600DCF7C45B18A0A6267535A1EBD3C` | `838F5BD09A080DECB26971D9E48F09B9494F62743EEC161E2DEAB81D00BFB498B55F3DA889B24B267FBF25DA8B6010DDB93211844761B3FDD27CDBB4FB472E9C` |
| `uvr-mdx-hq-3.onnx` | `317554B07FE1EA5279A77F2B1520A41EA4B93432560C4FFD08792C30FDDF9ADC` | `4B8E2D28EF6CA858C389B29BC0563E0CA9C5614AA548E487984ECEFF0875C1AFA3DD803CABD55E4A40EB73D492303AFDD4CF3BB4DAD0D25D352251599E63F62B` |

## ZIP bundles in 1.0.3

A manifest entry can now describe an installable ZIP bundle:

```json
{
  "name": "example.zip",
  "model_type": "Text to Speech",
  "used_for": "Speech synthesis",
  "supports": ["cpu"],
  "author": "Example",
  "sha256": "<package SHA-256>",
  "sha512": "<package SHA-512>",
  "package_kind": "zip",
  "install_dir": "tts/example",
  "required_paths": [
    "model.onnx",
    "tokens.txt"
  ]
}
```

For ZIP bundles, SHA-512 is required by the 1.0.3 installer and is checked **before** extraction. `required_paths` describes the runtime files/directories that must exist after extraction. A package may contain those files directly at archive root or inside one wrapper directory; the installer locates the bundle root automatically.

The ZIP is a transport/install artifact. Runtime applications should use the extracted paths under `install_dir`.

### Published package digests

| Package | SHA-256 | SHA-512 |
|---|---|---|
| `omnilingual-asr-300m-int8.zip` | `8CF7EE7A1F1EA080D442C92E38551F140D7DD943A57B02DA34282C80AFD6A747` | `82B7D136EDE03C38D90242F8F0D125FA3898EA79092254A19759997430E0800B200E5DAAAE06BFE5637D225E3C3C72EC76D87A782F277D6C5D17DD6B25F702A2` |
| `supertonic3.zip` | `2B61A419683CCB74627E451403DC52CC27F9B71A99DCE8A8FB978612821CBF12` | `676A8F3EB9711AF0315450BA679E902911FD14882834C81853D0C3F3B6575F72DBFA30FA772D0E87C56F94966BECFE809CC83E1E82CC1505DC04D92573F5A2D0` |
