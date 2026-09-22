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

## ZIP bundles in 1.0.3+

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

For ZIP bundles, SHA-512 is required by the installer and is checked **before** extraction. `required_paths` describes the runtime files/directories that must exist after extraction. A package may contain those files directly at archive root or inside one wrapper directory; the installer locates the bundle root automatically.

The ZIP is a transport/install artifact. Runtime applications should use the extracted paths under `install_dir`.

### Published package digests

| Package | SHA-256 | SHA-512 |
|---|---|---|
| `omnilingual-asr-300m-int8.zip` | `8CF7EE7A1F1EA080D442C92E38551F140D7DD943A57B02DA34282C80AFD6A747` | `82B7D136EDE03C38D90242F8F0D125FA3898EA79092254A19759997430E0800B200E5DAAAE06BFE5637D225E3C3C72EC76D87A782F277D6C5D17DD6B25F702A2` |
| `supertonic3.zip` | `2B61A419683CCB74627E451403DC52CC27F9B71A99DCE8A8FB978612821CBF12` | `676A8F3EB9711AF0315450BA679E902911FD14882834C81853D0C3F3B6575F72DBFA30FA772D0E87C56F94966BECFE809CC83E1E82CC1505DC04D92573F5A2D0` |


## Large Language Model bundles in 1.0.4–1.0.5

AI Model Manager 1.0.4 adds installable ONNX Runtime GenAI language-model bundles. They are verified as ZIP transport packages and extracted into a stable `llm/` location so desktop applications can resolve the same model path every time.

Required runtime files for the DeepSeek R1 bundles:

- `genai_config.json`
- `model.onnx`
- `model.onnx.data`
- `special_tokens_map.json`
- `tokenizer.json`
- `tokenizer_config.json`

Installed locations:

- `llm/deepseek-r1-1.5b`
- `llm/deepseek-r1-7b`

### Published DeepSeek package digests

| Package | SHA-256 | SHA-512 |
|---|---|---|
| `deepseek r1 - 1.5B.zip` | `0640D9A331C57B74DA6916E4345CE9A129E3B7AE250B029D171FDD4B40496C16` | `241E9174CC21B86EFE92C99B8D97E6A880011FE1CB04815D11E1EDBEB9D6DB6697F0EB0DEC34AEA89D775AA20AB65DEE77993B8FC3684826308C4CDABBC40C48` |
| `deepseek r1 - 7B.zip` | `E50EA151D9F45E97AB1608F1B6B86E18FEF46E7A026633C6EE1728B1545A869A` | `F04A9F1DDCBD4CCED45B709EECEA9365671D9BD08E00514F7DD84DC0ED0E76D25CF14C0C4C6AE8D1FB622186B972780D634583A5872CC3F8F369015BE480662C` |

MD5 and SHA-1 values supplied for these packages are retained only as historical/reference fingerprints; installation trust is based on SHA-512 (with SHA-256 also present in the manifest).

### DeepSeek R1 Distill Qwen 14B INT4 (1.0.5)

Transport package: `deepseek r1 - 14B.zip`

- Install directory: `llm/deepseek-r1-14b`
- SHA-256: `D7743EB57B631D257ECA2041FC09FFC6B9ACB5FB4FDB1A26AA6930A272BF03B9`
- SHA-512: `5FB54DAB430D86175C3BF1B01D8582039784B8193B3945B1576C56181CABBDFDE7257EF9801BD3248C47962B1784A54C26756456F166EA0F17E8C5DC1866B908`

Reference-only legacy digests (not used for `Verified`):

- MD5: `F681EFB63EA560AF54E8848C42943CA4`
- SHA-1: `7B8E2C0335F562468605B24DE20664C41DB964CE`

The extracted bundle must contain `genai_config.json`, `model.onnx`, `model.onnx.data`, `special_tokens_map.json`, `tokenizer.json`, and `tokenizer_config.json`. DeepSeek Local 0.5+ can discover this directory automatically through AI Model Manager's `ModelRoot`.

