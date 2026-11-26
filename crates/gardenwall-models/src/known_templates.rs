//! The reviewed chat templates (release plan feature 4, reviewed by the maintainer
//! on 2026-11-22). Each was read from Hugging Face at the commit named in
//! `source`, audited, and shown to the maintainer in full or as a diff against
//! the maker's template. The reviewed set: Qwen, Gemma, Llama 3.x,
//! Mistral, gpt-oss, Phi-4, DeepSeek and GLM 4.x; the maker's own template and
//! unsloth's and bartowski's exact copies, each by its own digest (over
//! [`normalize`](crate::template::normalize)d text), never "close enough".
//! `source` names one repository it was found in (the maker's when there is
//! one); the comment after an entry lists the other repositories read that
//! carry the same template.
//!
//! A new entry needs the same process: fetch at a pinned commit, audit, owner
//! review, then add it here and bump `template::AUDIT_VERSION`.

use crate::template::KnownTemplate;

pub(crate) const LIST: &[KnownTemplate] = &[
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "a55ee1b1660128b7098723e0abcd92caa0788061051c62d51cbe87d9cf1974d8",
        source: "Qwen/Qwen3-8B@b968826d9c46dd6066d109eabc6255188de91218",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "57f1fd00f0013a2be96aa79b857391f27e23df5b5f847072b524c897e24d0361",
        source: "Qwen/Qwen3-0.6B-GGUF@23749fefcc72300e3a2ad315e1317431b06b590a",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "64f85b198065d0fba2a81f37e10ed68161ce2c19a754c7100e67e0ca2ee9c326",
        source: "Qwen/Qwen3-4B-Instruct-2507@cdbee75f17c01a7cc42f958dc650907174af0554",
    }, // also: Qwen/Qwen3-30B-A3B-Instruct-2507, Qwen/Qwen3-Next-80B-A3B-Instruct, unsloth/Qwen3-Next-80B-A3B-Instruct-GGUF
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "3802169b2a02b81e6adb7ab4f64f91ff02db753c8c3a64a01c35192d3a61d8d7",
        source: "Qwen/Qwen3-30B-A3B-Thinking-2507@144afc2f379b542fdd4e85a1fcd5e1f79112d95d",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "32bf2564d2245b98298677d3ac2332ab50bc3279740c13c3974689464b38aa3c",
        source: "Qwen/Qwen3-Coder-30B-A3B-Instruct@b2cff646eb4bb1d68355c01b18ae02e7cf42d120",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "9d879ef91075acbfb2df7e441fd79d60bacfca989cf6d511d2af45c681e94f10",
        source: "Qwen/Qwen3-VL-8B-Instruct@0c351dd01ed87e9c1b53cbc748cba10e6187ff3b",
    }, // also: bartowski/Qwen_Qwen3-VL-8B-Instruct-GGUF, unsloth/Qwen3-VL-8B-Instruct-GGUF
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "5da44855ab7e0641774d5ed0ff0b4f89671d01df7d316e54d06557a962211cb4",
        source: "unsloth/Qwen3-8B-GGUF@a6adef130ffb23ddaf1a62fec9dced968c9bc482",
    }, // also: unsloth/Qwen3-4B-GGUF
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "c979e0e71a3e21b8f208e6ab120d5cb29327885f29d2a8b18fda67a723798e18",
        source: "unsloth/Qwen3-4B-Instruct-2507-GGUF@a06e946bb6b655725eafa393f4a9745d460374c9",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "40c21f34cf67d8c760ef72f8ad3ae5afad514299d4b06e91dd9a8d705af7b541",
        source: "unsloth/Qwen3-30B-A3B-Instruct-2507-GGUF@eea7b2be5805a5f151f8847ede8e5f9a9284bf77",
    }, // also: bartowski/Qwen_Qwen3-30B-A3B-Instruct-2507-GGUF, bartowski/Qwen_Qwen3-4B-Instruct-2507-GGUF
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "87710339d25b4e789c1d723f93c91ee861a86d305bb3d20a845536f251d6ea8a",
        source: "unsloth/Qwen3-Coder-30B-A3B-Instruct-GGUF@b17cb02dd882d5b6ab62fc777ad2995f19668350",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "5595e8741bb5500f9d395e05a0aefe6ca1e8d54c5dd8b98bcf7760d895fdc6e6",
        source: "bartowski/Qwen_Qwen3-8B-GGUF@0b69f75b7472688e6808490aa2b85efdb81b5ce7",
    },
    KnownTemplate {
        family: "Qwen 3",
        sha256_hex: "0b5c439b5b14126c0ae8b14ca93240c3cef1e7e29d684102b07f10727523dbdb",
        source: "bartowski/Qwen_Qwen3-Coder-Next-GGUF@d32741c4b434bf1f927798d0c093564c7f4e92fd",
    },
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "a4aee8afcf2e0711942cf848899be66016f8d14a889ff9ede07bca099c28f715",
        source: "Qwen/Qwen3.5-9B@c202236235762e1c871ad0ccb60c8ee5ba337b9a",
    }, // also: Qwen/Qwen3.5-27B, bartowski/Qwen_Qwen3.5-27B-GGUF, bartowski/Qwen_Qwen3.5-9B-GGUF
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "1b8361565cc35d47551d342214cff341c0f1a9685ce529c7ce31de0ed3616248",
        source: "Qwen/Qwen3.6-35B-A3B@345d467b117d1fd5eda9b879a5988de9ae5f47a7",
    }, // also: Qwen/Qwen3.6-27B
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "af16f654968d47d09a6c604e00370b12dbc21f76456f59c9e98b82e84d9f6375",
        source: "Qwen/Qwen3.8-27B@37561816e66b5b9153b1b64c10968c18c1e6f4a0",
    }, // also: Qwen/Qwen3.8-Flash-Next
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "7f0e529032c25183bcd66c7f238da2d377f43be754a94e2725a58c4e16d2ed67",
        source: "unsloth/Qwen3.5-9B-GGUF@3885219b6810b007914f3a7950a8d1b469d598a5",
    }, // also: unsloth/Qwen3.5-4B-GGUF
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "e60df41481b6ad20571cda7e2e1290cfee25670f1b50973f74576c9354c66336",
        source: "unsloth/Qwen3.5-27B-GGUF@3221f178a6b842d04f1fb42f1c413534adcc0a6a",
    }, // also: unsloth/Qwen3.5-35B-A3B-GGUF
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "55d4931433fe502b794226ee7f4d206a6bdd436ac9f80eb7d8ebb4c639f9ea0c",
        source: "unsloth/Qwen3.6-35B-A3B-GGUF@a483e9e6cbd595906af30beda3187c2663a1118c",
    }, // also: unsloth/Qwen3.6-27B-GGUF
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "12827f24b742ea4e80cdc12dbcf9622227056b9f797252a3149263d4f9aaadce",
        source: "unsloth/Qwen3.8-27B-GGUF@4af51b3542f775b0a8e4b310bfb3f17e01c306c9",
    },
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "c3cf9e34abf4f9e36c2d72165aa9c132d3e2a725b6c2586aaa3a8af9d7a81041",
        source: "unsloth/Qwen3.8-Flash-Next-GGUF@f5d175de2cf7f720d45d6064019a81fcf056e986",
    }, // also: bartowski/Qwen3.8-27B-GGUF, bartowski/Qwen3.8-Flash-Next-GGUF
    KnownTemplate {
        family: "Qwen 3.5-3.8",
        sha256_hex: "e84f32a23fdda27689f868aa4a1a5621f41133e51a48d7f3efcbea2839574259",
        source: "bartowski/Qwen_Qwen3.6-35B-A3B-GGUF@5c2410d71524f4f72b023ce8daf7a80528226d5f",
    }, // also: bartowski/Qwen_Qwen3.6-27B-GGUF
    KnownTemplate {
        family: "Gemma 3",
        sha256_hex: "8d9e3a3c114fb2053ec500184677af4f7e5f8ec2582443cd535834e68b3ade29",
        source: "unsloth/gemma-3-12b-it-GGUF@d15e4c7dc21dc55d56bf8549db57a71ad8a2a35d",
    }, // also: bartowski/google_gemma-3-12b-it-GGUF, bartowski/google_gemma-3-27b-it-GGUF, bartowski/google_gemma-3-4b-it-GGUF, unsloth/gemma-3-27b-it-GGUF, unsloth/gemma-3-4b-it-GGUF
    KnownTemplate {
        family: "Gemma 4",
        sha256_hex: "6a1015c47ccfcfa67c3b772385bccee357a4d37c3cda37bd202e9047f391ab82",
        source: "google/gemma-4-12B-it@43c9d014b231474fa7212dbb48a762566c2b255d",
    }, // also: bartowski/gemma-4-12B-it-GGUF, bartowski/google_gemma-4-26B-A4B-it-GGUF, bartowski/google_gemma-4-31B-it-GGUF, google/gemma-4-12B-it-qat-q4_0-gguf, google/gemma-4-26B-A4B-it, google/gemma-4-26B-A4B-it-qat-q4_0-gguf, google/gemma-4-31B-it
    KnownTemplate {
        family: "Gemma 4",
        sha256_hex: "603a42db292c25278e9b23d94c7abbe77453f13e26a053c43c5c2f900b4136f7",
        source: "google/gemma-4-E4B-it@4ecc2ba63bcac23ba4a767fc87025b1450f84c9c",
    }, // also: bartowski/google_gemma-4-E4B-it-GGUF, google/gemma-4-E4B-it-qat-q4_0-gguf
    KnownTemplate {
        family: "Gemma 4",
        sha256_hex: "aa3185dfc65051046349995de1cfc2fbb275491b9a3832b71595f1ff5b31d61b",
        source: "unsloth/gemma-4-12b-it-GGUF@fc034cfff751157913579611efad8462ac1be606",
    }, // also: unsloth/gemma-4-26B-A4B-it-GGUF, unsloth/gemma-4-31B-it-GGUF
    KnownTemplate {
        family: "Gemma 4",
        sha256_hex: "74a88f94c57c14e22dcdbe08e94127f42b87c6f584cb73838e73e32126f3f6b3",
        source: "unsloth/gemma-4-E4B-it-GGUF@bfc15c382204943c3a8fff0c750b94ae2364d7a3",
    }, // also: unsloth/gemma-4-E2B-it-GGUF
    KnownTemplate {
        family: "Llama 3.x",
        sha256_hex: "93c0e9aa3629bbd77e68dbc0f5621f6e6b23aa8d74b932595cdb8d64684526d7",
        source: "unsloth/Llama-3.1-8B-Instruct-GGUF@600b0020115fd6b17f0752848fe7b5a1be686bcd",
    }, // also: bartowski/Llama-3.3-70B-Instruct-GGUF, bartowski/Meta-Llama-3.1-8B-Instruct-GGUF, unsloth/Llama-3.3-70B-Instruct-GGUF
    KnownTemplate {
        family: "Llama 3.x",
        sha256_hex: "d82792f95932f1c9cef5c4bd992f171225e3bf8c7b609b4557c9e1ec96be819f",
        source: "unsloth/Llama-3.2-3B-Instruct-GGUF@e7d0997e49c9cb00d88b4c1a6a16aa894b0bbc31",
    }, // also: bartowski/Llama-3.2-1B-Instruct-GGUF, bartowski/Llama-3.2-3B-Instruct-GGUF, unsloth/Llama-3.2-1B-Instruct-GGUF
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "5479d873a2555271fee929ff941c1bc1f1c198609bab7fa0a05f356692664236",
        source: "mistralai/Ministral-3-8B-Instruct-2512@5b26027e7b19eeb4b7352e1fed3926375dd2cb4d",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "54d75870b562f77f1cb0fc11164477fe1fd17628a4227b4e302335cfc3b2ef8b",
        source: "mistralai/Ministral-3-8B-Instruct-2512-GGUF@0102285ad796bd99af90f58de616092e5630e970",
    }, // also: bartowski/mistralai_Ministral-3-8B-Instruct-2512-GGUF
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "31b50c917746ef1c2611c2fef122d3ebee690aca5748ab37832734f9ad375da1",
        source: "mistralai/Ministral-3-14B-Reasoning-2512-GGUF@fe3b038f30334729263d860d5dadbaa34e0f2a18",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "9de879b92d707653a18fab2f76881cc984e293389dbf2a1dcf092ec06b2840c9",
        source: "mistralai/Devstral-Small-2507_gguf@ee2f0c00c5c86862f471fbf533268cf01b80d4a6",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "e7deee034838db2bfc7487788a3013d8a307ab69f72f3c54a85f06fd76007d4e",
        source: "mistralai/Mistral-Nemo-Instruct-2407@04d8a90549d23fc6bd7f642064003592df51e9b3",
    }, // also: bartowski/Mistral-Nemo-Instruct-2407-GGUF
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "a126eea7996a0ac9540b2fc7fb4af1dad975d3b388c194c713921f2b4205aa19",
        source: "unsloth/Mistral-Small-3.2-24B-Instruct-2506-GGUF@b750ec2299225e492f1bd27cab88a0a595fa848f",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "2ef2939ee9623cd56449b410d89405e43094f2fee620dc6395e67409124c51f3",
        source: "unsloth/Ministral-3-8B-Instruct-2512-GGUF@3731507ec3e867db16d620f73e14d689125758f4",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "60f2c46b196c943f09726ce8b0abd1de152888d8dc83b7fd16c5725c125b71dd",
        source: "unsloth/Ministral-3-14B-Instruct-2512-GGUF@9b948841b705cc11285ff274b84ba6c885908b19",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "87989798313de539b55bdeb19c98214e1a7bdd8c3a62b7d7cf4138d58ffdbeb5",
        source: "unsloth/Ministral-3-14B-Reasoning-2512-GGUF@2d687ca0523d4d87469835433b53642dfae83152",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "adb1c45376c8a8a1b7e0063ce87b26c8b9376fd4f0f3cb584ddbba7e66f81d2f",
        source: "unsloth/Devstral-Small-2-24B-Instruct-2512-GGUF@6e458b8add42681bfd023de5eab93637694aaf82",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "3f6683b4cea89122155ef307007486bc39f10b91e36446b9712db85b1bddc6a1",
        source: "unsloth/Magistral-Small-2509-GGUF@77fe877ee8e44eadf97235a1ad77471678eed310",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "ac888ea51c72437ddbd57032b819c5d04d76c313147e1f3f2add070493fac49c",
        source: "unsloth/Mistral-Nemo-Instruct-2407-GGUF@6ce5144b3ed09c5cf26c7c127f7fb2e67de659d3",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "d990bccdaaa20611e261434bf60bce767b03d8ee1f1d9b5cd0742b1cc96066b9",
        source: "bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF@b3592d09be469bedab6be51f577824b3b6b76ba1",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "8ef742b9d31ce5def18cf658fcf7da0cdcea99d4718ba6926130c1c25f255187",
        source: "bartowski/mistralai_Ministral-3-14B-Instruct-2512-GGUF@9289ba93995b2a15fd72b1a9fe4f85697199ed1f",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "ed825380b678dad25046e0a92ef5c64a57178ccaba7e780b5ed581e91a4fd6ac",
        source: "bartowski/mistralai_Devstral-Small-2-24B-Instruct-2512-GGUF@027695770ae1de77c2f6fb19f8e1ba9d65fcd15d",
    },
    KnownTemplate {
        family: "Mistral",
        sha256_hex: "8f78fa112de03884b525f9c48ec1d1c411daba0440c700e874f75ac319218303",
        source: "bartowski/mistralai_Magistral-Small-2509-GGUF@0cb9140d941601fe6435f60443e8e10532f7e01c",
    },
    KnownTemplate {
        family: "gpt-oss",
        sha256_hex: "a4c9919cbbd4acdd51ccffe22da049264b1b73e59055fa58811a99efbd7c8146",
        source: "openai/gpt-oss-20b@6cee5e81ee83917806bbde320786a8fb61efebee",
    }, // also: openai/gpt-oss-120b
    KnownTemplate {
        family: "gpt-oss",
        sha256_hex: "70da0d2348e40aaf8dad05f04a316835fd10547bd7e3392ce337e4c79ba91c01",
        source: "unsloth/gpt-oss-20b-GGUF@d449b42d93e1c2c7bda5312f5c25c8fb91dfa9b4",
    },
    KnownTemplate {
        family: "gpt-oss",
        sha256_hex: "acaa726974dadda7844c6607c59554bca390645b94705cfdc2e78bb34895717c",
        source: "unsloth/gpt-oss-120b-GGUF@ff1a82da6ad466e32284fa3d2b86694db3204789",
    },
    KnownTemplate {
        family: "gpt-oss",
        sha256_hex: "f8d9255777615591a7cc1a7c932f5a69e181128902295e1b81221d20d983cac7",
        source: "bartowski/openai_gpt-oss-20b-GGUF@e39ba3aa000c47c83dacdc9e1ca2c9dd0808c205",
    },
    KnownTemplate {
        family: "gpt-oss",
        sha256_hex: "b474759b33c21342e73abceae7fe3789f062e289f69749801f722a508f92180a",
        source: "bartowski/openai_gpt-oss-120b-GGUF@8d64014efc314685c68ad448590f905fcb20e534",
    },
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "fe4120415a3858b873955c92ce3a103a95bc97c2cb00b72e62659aaf0f891ee5",
        source: "microsoft/phi-4@2db69c1c3e91a05d2c64a3185acfbaf36f744e25",
    }, // also: bartowski/phi-4-GGUF, microsoft/phi-4-gguf, unsloth/phi-4-GGUF
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "febf589225c9728ab791f52e8897d7607a823d45368f0a4c92fa68997b40cce9",
        source: "microsoft/Phi-4-mini-instruct@cfbefacb99257ffa30c83adab238a50856ac3083",
    }, // also: bartowski/microsoft_Phi-4-mini-instruct-GGUF
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "5a4c7c0b12bdbbc7110aebe80b15ac3d4dbabde85be466925dc2cf52d7698ceb",
        source: "microsoft/Phi-4-reasoning-plus@69baf8528e1bcf05f475034d9e5dd32875ed125f",
    },
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "cbb504cb43fec3f8877d709d9f7a729441e56f41cb4fddd382675fab35de7fe1",
        source: "microsoft/Phi-4-mini-reasoning@0e3b1e2d02ee478a3743abe3f629e9c0cb722e0a",
    }, // also: bartowski/microsoft_Phi-4-mini-reasoning-GGUF, unsloth/Phi-4-mini-reasoning-GGUF
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "46bcd040271c497ee454c7ff577615abf2853ba2971b472e417468ac2bf47f52",
        source: "unsloth/Phi-4-mini-instruct-GGUF@78eb92a46fc37e6b524df991ed9aca9bc6aa7b80",
    },
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "f713f8fb425437f11bed1b4c25b3e4531a572ab6adb85e68d1f09669f4c2f2e9",
        source: "unsloth/Phi-4-reasoning-plus-GGUF@80fff8542dc7b88dba725b660beefd80e91e80c9",
    },
    KnownTemplate {
        family: "Phi-4",
        sha256_hex: "4e88dcaabb2b09ddb706f789a9ca66bc60dc4cb05fc142ae20b418b46198829e",
        source: "bartowski/microsoft_Phi-4-reasoning-plus-GGUF@96b602db19694b8323c8638ad771289fb5be0863",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "56a1447ad31926fdc21fb07e56e5642bd9c850c4f52d8c8af7bbe5f079a84f5f",
        source: "deepseek-ai/DeepSeek-R1-Distill-Qwen-14B@1df8507178afcc1bef68cd8c393f61a886323761",
    }, // also: deepseek-ai/DeepSeek-R1-Distill-Llama-8B, unsloth/DeepSeek-R1-Distill-Llama-8B-GGUF
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "53671bac29bcb3e1f95f51b8c77d85a6011b66dc38bd23dce64eef04384a976b",
        source: "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B@6e8885a6ff5c1dc5201574c8fd700323f23c25fa",
    }, // also: bartowski/deepseek-ai_DeepSeek-R1-0528-Qwen3-8B-GGUF
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "45690185067ea25c073bff2d3e425a45ee2564d2bbf6cdf7d885e139d460b8e0",
        source: "deepseek-ai/DeepSeek-V3.1@c0781d039fb7a1ba2abc4add0bdc293e92d2b8db",
    }, // also: bartowski/deepseek-ai_DeepSeek-V3.1-GGUF
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "040344d19ffaf30e013dd96ce4dac5d9637a0bb599702bddf4b18820adf8867f",
        source: "deepseek-ai/DeepSeek-V4.1-Flash-1017@08c8657f02291e11b6f6b63e0e16014f8e748518",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "b6835114b7303ddd78919a82e4d9f7d8c26ed0d7dfc36beeb12d524f6144eab1",
        source: "unsloth/DeepSeek-R1-Distill-Qwen-14B-GGUF@7b05b58b41f623e66fc74cd27b35475267b2f3e3",
    }, // also: bartowski/DeepSeek-R1-Distill-Llama-8B-GGUF, bartowski/DeepSeek-R1-Distill-Qwen-14B-GGUF, bartowski/DeepSeek-R1-Distill-Qwen-7B-GGUF, unsloth/DeepSeek-R1-Distill-Qwen-7B-GGUF
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "97ee3167ef705d0e9e5dc1610dd019322c60b51dd3823ad51eb5562092e320bc",
        source: "unsloth/DeepSeek-R1-0528-Qwen3-8B-GGUF@eb48357c179d34dbf515983f798dfb8752a0f261",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "eccaf95e65e90df3890fc2a48dea6f287c725a7385a9a0cdb0749a8fba8ce0b2",
        source: "unsloth/DeepSeek-V3.1-GGUF@feb73effeee56ec0e5733a38c825d8fec1d76314",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "66f38a3bf2e0e032a5a58b5d1356d44b227c1d3bd6c52fc17f33a577cef3e303",
        source: "unsloth/DeepSeek-V3.2-GGUF@a787696863bafd5c736955ef81cc869a0bf6178a",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "d05566ebe26667ec54f4ef7a3dbc114ce2e00aefc40e0c62286374eee0e22080",
        source: "unsloth/DeepSeek-V4-Flash-GGUF@e3aa0d6a5fa4f820d9e132ac1fd1d01e1b2b49e0",
    },
    KnownTemplate {
        family: "DeepSeek",
        sha256_hex: "a4b1fecbe5e06548221a301c5432ab70434a06848e6f6253177578f4a7a953bc",
        source: "unsloth/DeepSeek-V4.1-Flash-1017-GGUF@dfcee0ec8d6b874c6663eca0fc55d90ab32534c8",
    },
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "d63ad536c3c81880043e22ec7fd08db42b4d8fb7c89c7138bc562bfa25281375",
        source: "zai-org/GLM-4.7-Flash@7dd20894a642a0aa287e9827cb1a1f7f91386b67",
    }, // also: bartowski/zai-org_GLM-4.7-Flash-GGUF, unsloth/GLM-4.7-Flash-GGUF, zai-org/GLM-4.7
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "44f815868bf02fa458dd2f741a338046f4bf45f398eb6d067766726b9d96cce3",
        source: "zai-org/GLM-4.5-Air@a24ceef6ce4f3536971efe9b778bdaa1bab18daa",
    }, // also: zai-org/GLM-4.6
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "db700f25fa300e53634c6fc78dee86b7fbd6d27e624edb855b18a4078c83a822",
        source: "zai-org/GLM-4-9B-0414@645b8482494e31b6b752272bf7f7f273ef0f3caf",
    }, // also: zai-org/GLM-4-32B-0414
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "15a206ef798293a877c992af1ea4c5aac10dbbda1074ee8acda12f894ca55fb4",
        source: "unsloth/GLM-4.5-Air-GGUF@506d64aa8c5cfe9dbbf00bc7a15739438f83204d",
    }, // also: unsloth/GLM-4.6-GGUF
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "03bf35977223e0dab55297aae34c0af7da46fc2ee6ec82bb24b2bb98f902cd43",
        source: "unsloth/GLM-4.7-GGUF@70deda3fcbe622671ff2111d7c0c7392f618e3b0",
    },
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "e81f91f841610ef39de851528d6ea42e0fd9499a3e01e4843e015888dd3b6aae",
        source: "unsloth/GLM-4-9B-0414-GGUF@40a17a0c8f24664ddd851dafd03935553c25a57e",
    }, // also: unsloth/GLM-4-32B-0414-GGUF
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "c5b4f04ea931bbf75997887958997737ca053fc040d6090907b16447440c22e1",
        source: "bartowski/zai-org_GLM-4.5-Air-GGUF@bd907156c1ca4dd837540eda77043fddfd6d5caf",
    },
    KnownTemplate {
        family: "GLM 4.x",
        sha256_hex: "854b703e44ca06bdb196cc471c728d15dbab61e744fe6cdce980086b61646ed1",
        source: "bartowski/THUDM_GLM-4-9B-0414-GGUF@211924cd949e153b1017ed635e5b127d0b594e6f",
    }, // also: bartowski/THUDM_GLM-4-32B-0414-GGUF
];
