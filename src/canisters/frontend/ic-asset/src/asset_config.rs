use anyhow::{bail, Context};
use candid::CandidType;
use derivative::Derivative;
use globset::{Glob, GlobMatcher};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) const ASSETS_CONFIG_FILENAME_JSON: &str = ".ic-assets.json";
pub(crate) const ASSETS_CONFIG_FILENAME_JSON5: &str = ".ic-assets.json5";

pub(crate) type HeadersConfig = HashMap<String, String>;
type ConfigMap = HashMap<PathBuf, Arc<AssetConfigTreeNode>>;

#[derive(Deserialize, Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CacheConfig {
    pub(crate) max_age: Option<u64>,
}

#[derive(CandidType, Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct RedirectUrl {
    pub(crate) host: Option<String>,
    pub(crate) path: Option<String>,
}

impl<'de> Deserialize<'de> for RedirectUrl {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        if value.is_object() {
            let mut host = None;
            let mut path = None;
            for (key, value) in value.as_object().unwrap() {
                match key.as_str() {
                    "host" => host = Some(value.as_str().unwrap().to_string()),
                    "path" => path = Some(value.as_str().unwrap().to_string()),
                    _ => {
                        return Err(serde::de::Error::custom(format!(
                            "Unexpected key: {:?}",
                            key
                        )))
                    }
                }
            }
            if host.is_none() && path.is_none() {
                return Err(serde::de::Error::custom(
                    "Expected at least one of host or path".to_string(),
                ));
            }
            Ok(RedirectUrl { host, path })
        } else {
            Err(serde::de::Error::custom(format!(
                "Expected object, found: {:?}",
                value
            )))
        }
    }
}

#[derive(Deserialize, CandidType, Serialize, Debug, Default, Clone, PartialEq, Eq)]
pub struct RedirectConfig {
    from: Option<RedirectUrl>,
    to: RedirectUrl,
    user_agent: Option<Vec<String>>,
    #[serde(default = "default_response_code")]
    response_code: u16,
}

fn default_response_code() -> u16 {
    308
}

#[derive(Derivative)]
#[derivative(Debug)]
struct AssetConfigRule {
    #[derivative(Debug(format_with = "fmt_glob_field"))]
    r#match: GlobMatcher,
    cache: Option<CacheConfig>,
    headers: Maybe<HeadersConfig>,
    ignore: Option<bool>,
    redirect: Option<RedirectConfig>, // TODO: consider this to be Vec<Option<RedirectConfig>>
}

#[derive(Deserialize, Debug)]
enum Maybe<T> {
    Null,
    Absent,
    Value(T),
}

fn fmt_glob_field(
    field: &GlobMatcher,
    formatter: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    formatter.write_str(field.glob().glob())?;
    Ok(())
}

impl AssetConfigRule {
    fn applies(&self, canonical_path: &Path) -> bool {
        // TODO: better dot files/dirs handling, awaiting upstream changes:
        // https://github.com/BurntSushi/ripgrep/issues/2229
        self.r#match.is_match(canonical_path)
    }
}

#[derive(Debug)]
pub(crate) struct AssetSourceDirectoryConfiguration {
    config_map: ConfigMap,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize, Clone)]
pub(crate) struct AssetConfig {
    pub(crate) cache: Option<CacheConfig>,
    pub(crate) headers: Option<HeadersConfig>,
    pub(crate) ignore: Option<bool>,
    pub(crate) redirect: Option<RedirectConfig>,
}

impl std::fmt::Display for AssetConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();

        if self.cache.is_some() || self.headers.is_some() || self.redirect.is_some() {
            s.push_str(", with config:\n");
        }
        if let Some(ref redirect) = self.redirect {
            s.push_str(&format!("    - redirect: {:?}\n", redirect));
        }
        if let Some(ref cache) = self.cache {
            s.push_str(&format!("    - cache: {:?}\n", cache));
        }
        if let Some(ref headers) = self.headers {
            for (key, value) in headers {
                s.push_str(&format!(
                    "    - header: {key}: {value}\n",
                    key = key,
                    value = value
                ));
            }
        }

        write!(f, "{}", s)
    }
}

#[derive(Debug, Default)]
struct AssetConfigTreeNode {
    pub parent: Option<Arc<AssetConfigTreeNode>>,
    pub rules: Vec<AssetConfigRule>,
}

impl AssetSourceDirectoryConfiguration {
    /// Constructs config tree for assets directory.
    pub(crate) fn load(root_dir: &Path) -> anyhow::Result<Self> {
        if !root_dir.has_root() {
            bail!("root_dir paramenter is expected to be canonical path")
        }
        let mut config_map = HashMap::new();
        AssetConfigTreeNode::load(None, root_dir, &mut config_map)?;

        Ok(Self { config_map })
    }

    pub(crate) fn get_asset_config(&self, canonical_path: &Path) -> anyhow::Result<AssetConfig> {
        let parent_dir = canonical_path.parent().with_context(|| {
            format!(
                "unable to get the parent directory for asset path: {:?}",
                canonical_path
            )
        })?;
        Ok(self
            .config_map
            .get(parent_dir)
            .with_context(|| {
                format!(
                    "unable to find asset config for following path: {:?}",
                    parent_dir
                )
            })?
            .get_config(canonical_path))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterimAssetConfigRule {
    r#match: String,
    cache: Option<CacheConfig>,
    #[serde(default, deserialize_with = "deser_headers")]
    headers: Maybe<HeadersConfig>,
    ignore: Option<bool>,
    redirect: Option<RedirectConfig>,
}

impl<T> Default for Maybe<T> {
    fn default() -> Self {
        Self::Absent
    }
}

fn deser_headers<'de, D>(deserializer: D) -> Result<Maybe<HeadersConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match serde_json::value::Value::deserialize(deserializer)? {
        Value::Object(v) => Ok(Maybe::Value(
            v.into_iter()
                .map(|(k, v)| (k, v.to_string().trim_matches('"').to_string()))
                .collect::<HashMap<String, String>>(),
        )),
        Value::Null => Ok(Maybe::Null),
        _ => Err(serde::de::Error::custom(
            "wrong data format for field `headers` (only map or null are allowed)",
        )),
    }
}

impl AssetConfigRule {
    fn from_interim(
        InterimAssetConfigRule {
            r#match,
            cache,
            headers,
            ignore,
            redirect,
        }: InterimAssetConfigRule,
        config_file_parent_dir: &Path,
    ) -> anyhow::Result<Self> {
        let glob = Glob::new(
            config_file_parent_dir
                .join(&r#match)
                .to_str()
                .with_context(|| {
                    format!(
                        "cannot combine {} and {} into a string (to be later used as a glob pattern)",
                        config_file_parent_dir.display(),
                        r#match
                    )
                })?,
        )
        .with_context(|| format!("{} is not a valid glob pattern", r#match))?.compile_matcher();

        Ok(Self {
            r#match: glob,
            cache,
            headers,
            ignore,
            redirect,
        })
    }
}

impl AssetConfigTreeNode {
    fn load(
        parent: Option<Arc<AssetConfigTreeNode>>,
        dir: &Path,
        configs: &mut HashMap<PathBuf, Arc<AssetConfigTreeNode>>,
    ) -> anyhow::Result<()> {
        let config_path: Option<PathBuf>;
        match (
            dir.join(ASSETS_CONFIG_FILENAME_JSON).exists(),
            dir.join(ASSETS_CONFIG_FILENAME_JSON5).exists(),
        ) {
            (true, true) => {
                return Err(anyhow::anyhow!(
                    "both {} and {} files exist in the same directory (dir = {:?})",
                    ASSETS_CONFIG_FILENAME_JSON,
                    ASSETS_CONFIG_FILENAME_JSON5,
                    dir
                ))
            }
            (true, false) => config_path = Some(dir.join(ASSETS_CONFIG_FILENAME_JSON)),

            (false, true) => config_path = Some(dir.join(ASSETS_CONFIG_FILENAME_JSON5)),
            (false, false) => config_path = None,
        }
        let mut rules = vec![];
        if let Some(config_path) = config_path {
            let content = fs::read_to_string(&config_path).with_context(|| {
                format!("unable to read config file: {}", config_path.display())
            })?;
            let interim_rules: Vec<InterimAssetConfigRule> = json5::from_str(&content)
                .with_context(|| {
                    format!(
                        "malformed JSON asset config file: {}",
                        config_path.display()
                    )
                })?;
            for interim_rule in interim_rules {
                rules.push(AssetConfigRule::from_interim(interim_rule, dir)?);
            }
        }

        let parent_ref = match parent {
            Some(p) if rules.is_empty() => p,
            _ => Arc::new(Self { parent, rules }),
        };

        configs.insert(dir.to_path_buf(), parent_ref.clone());
        for f in std::fs::read_dir(&dir)
            .with_context(|| format!("Unable to read directory {}", &dir.display()))?
            .filter_map(|x| x.ok())
            .filter(|x| x.file_type().map_or_else(|_e| false, |ft| ft.is_dir()))
        {
            Self::load(Some(parent_ref.clone()), &f.path(), configs)?;
        }
        Ok(())
    }

    fn get_config(&self, canonical_path: &Path) -> AssetConfig {
        let base_config = match &self.parent {
            Some(parent) => parent.get_config(canonical_path),
            None => AssetConfig::default(),
        };
        self.rules
            .iter()
            .filter(|rule| rule.applies(canonical_path))
            .fold(base_config, |acc, x| acc.merge(x))
    }
}

impl AssetConfig {
    fn merge(mut self, other: &AssetConfigRule) -> Self {
        if let Some(c) = &other.cache {
            self.cache = Some(c.to_owned());
        };
        if let Some(c) = &other.redirect {
            self.redirect = Some(c.to_owned());
        };
        match (self.headers.as_mut(), &other.headers) {
            (Some(sh), Maybe::Value(oh)) => sh.extend(oh.to_owned()),
            (None, Maybe::Value(oh)) => self.headers = Some(oh.to_owned()),
            (_, Maybe::Null) => self.headers = None,
            (_, Maybe::Absent) => (),
        };

        if other.ignore.is_some() {
            self.ignore = other.ignore;
        }
        self
    }
}

#[cfg(test)]
mod with_tempdir {

    use super::*;
    use std::io::Write;
    #[cfg(target_family = "unix")]
    use std::os::unix::prelude::PermissionsExt;
    use std::{collections::BTreeMap, fs::File};
    use tempfile::{Builder, TempDir};

    fn create_temporary_assets_directory(
        config_files: Option<HashMap<String, String>>,
        assets_count: usize,
    ) -> anyhow::Result<TempDir> {
        let assets_dir = Builder::new().prefix("assets").rand_bytes(5).tempdir()?;

        let _subdirs = ["css", "js", "nested/deep"]
            .map(|d| assets_dir.as_ref().join(d))
            .map(std::fs::create_dir_all);

        let _asset_files = [
            "index.html",
            "js/index.js",
            "js/index.map.js",
            "css/main.css",
            "css/stylish.css",
            "nested/the-thing.txt",
            "nested/deep/the-next-thing.toml",
        ]
        .iter()
        .map(|path| assets_dir.path().join(path))
        .take(assets_count)
        .for_each(|path| {
            File::create(path).unwrap();
        });

        let new_empty_config = |directory: &str| (directory.to_string(), "[]".to_string());
        let mut h = HashMap::from([
            new_empty_config(""),
            new_empty_config("css"),
            new_empty_config("js"),
            new_empty_config("nested"),
            new_empty_config("nested/deep"),
        ]);
        if let Some(cf) = config_files {
            h.extend(cf);
        }
        h.into_iter().for_each(|(dir, content)| {
            let path = assets_dir
                .path()
                .join(dir)
                .join(ASSETS_CONFIG_FILENAME_JSON);
            let mut file = File::create(path).unwrap();
            write!(file, "{}", content).unwrap();
        });

        Ok(assets_dir)
    }

    #[test]
    fn match_only_nested_files() -> anyhow::Result<()> {
        let cfg = HashMap::from([(
            "nested".to_string(),
            r#"[{"match": "*", "cache": {"max_age": 333}}]"#.to_string(),
        )]);
        let assets_temp_dir = create_temporary_assets_directory(Some(cfg), 7).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;

        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        for f in ["nested/the-thing.txt", "nested/deep/the-next-thing.toml"] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig {
                    cache: Some(CacheConfig { max_age: Some(333) }),
                    ..Default::default()
                }
            );
        }
        for f in [
            "index.html",
            "js/index.js",
            "js/index.map.js",
            "css/main.css",
            "css/stylish.css",
        ] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig::default()
            );
        }

        Ok(())
    }

    #[test]
    fn redirects() -> anyhow::Result<()> {
        let cfg = HashMap::from([(
            "".to_string(),
            r#"[
                {
                  "match": "*",
                  "redirect": {
                    "from": {"host": "raw.ic0.app"},
                    "to": {"host": "ic0.app" },
                    "response_code": 301,
                    "user_agent": ["CrawlerBot"]
            }}]"#
                .to_string(),
        )]);
        let assets_temp_dir = create_temporary_assets_directory(Some(cfg), 7).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;

        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        for f in [
            "index.html",
            "js/index.js",
            "js/index.map.js",
            "css/main.css",
            "css/stylish.css",
        ] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig {
                    cache: None,
                    headers: None,
                    ignore: None,
                    redirect: Some(RedirectConfig {
                        from: Some(RedirectUrl {
                            host: Some("raw.ic0.app".to_string()),
                            path: None
                        }),
                        to: RedirectUrl {
                            host: Some("ic0.app".to_string()),
                            path: None
                        },
                        user_agent: Some(vec!["CrawlerBot".to_string()]),
                        response_code: 301
                    })
                }
            );
        }

        Ok(())
    }

    #[test]
    fn overriding_cache_rules() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([
            (
                "nested".to_string(),
                r#"[{"match": "*", "cache": {"max_age": 111}}]"#.to_string(),
            ),
            (
                "".to_string(),
                r#"[{"match": "*", "cache": {"max_age": 333}}]"#.to_string(),
            ),
        ]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 7).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;

        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        for f in ["nested/the-thing.txt", "nested/deep/the-next-thing.toml"] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig {
                    cache: Some(CacheConfig { max_age: Some(111) }),
                    ..Default::default()
                }
            );
        }
        for f in [
            "index.html",
            "js/index.js",
            "js/index.map.js",
            "css/main.css",
            "css/stylish.css",
        ] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig {
                    cache: Some(CacheConfig { max_age: Some(333) }),
                    ..Default::default()
                }
            );
        }

        Ok(())
    }

    #[test]
    fn overriding_headers() -> anyhow::Result<()> {
        use serde_json::Value::*;
        let cfg = Some(HashMap::from([(
            "".to_string(),
            r#"
    [
      {
        "match": "index.html",
        "cache": {
          "max_age": 22
        },
        "headers": {
          "Content-Security-Policy": "add",
          "x-frame-options": "NONE",
          "x-content-type-options": "nosniff"
        }
      },
      {
        "match": "*",
        "headers": {
          "Content-Security-Policy": "delete"
        }
      },
      {
        "match": "*",
        "headers": {
          "Some-Other-Policy": "add"
        }
      },
      {
        "match": "*",
        "cache": {
          "max_age": 88
        },
        "headers": {
          "x-xss-protection": 1,
          "x-frame-options": "SAMEORIGIN"
        }
      }
    ]
    "#
            .to_string(),
        )]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 1).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        let parsed_asset_config =
            assets_config.get_asset_config(assets_dir.join("index.html").as_path())?;
        let expected_asset_config = AssetConfig {
            cache: Some(CacheConfig { max_age: Some(88) }),
            headers: Some(HashMap::from([
                ("x-content-type-options".to_string(), "nosniff".to_string()),
                ("x-frame-options".to_string(), "SAMEORIGIN".to_string()),
                ("Some-Other-Policy".to_string(), "add".to_string()),
                ("Content-Security-Policy".to_string(), "delete".to_string()),
                (
                    "x-xss-protection".to_string(),
                    Number(serde_json::Number::from(1)).to_string(),
                ),
            ])),
            ..Default::default()
        };

        assert_eq!(parsed_asset_config.cache, expected_asset_config.cache);
        assert_eq!(
            parsed_asset_config
                .headers
                .unwrap()
                .iter()
                // keys are sorted
                .collect::<BTreeMap<_, _>>(),
            expected_asset_config
                .headers
                .unwrap()
                .iter()
                .collect::<BTreeMap<_, _>>(),
        );

        Ok(())
    }

    #[test]
    fn prioritization() -> anyhow::Result<()> {
        // 1. the most deeply nested config file takes precedens over the one in parent dir
        // 2. order of rules withing file matters - last rule in config file takes precedens over the first one
        let cfg = Some(HashMap::from([
            (
                "".to_string(),
                r#"[
        {"match": "**/*", "cache": {"max_age": 999}},
        {"match": "nested/**/*", "cache": {"max_age": 900}},
        {"match": "nested/deep/*", "cache": {"max_age": 800}},
        {"match": "nested/**/*.toml","cache": {"max_age": 700}}
    ]"#
                .to_string(),
            ),
            (
                "nested".to_string(),
                r#"[
        {"match": "the-thing.txt", "cache": {"max_age": 600}},
        {"match": "*.txt", "cache": {"max_age": 500}},
        {"match": "*", "cache": {"max_age": 400}}
    ]"#
                .to_string(),
            ),
            (
                "nested/deep".to_string(),
                r#"[
        {"match": "**/*", "cache": {"max_age": 300}},
        {"match": "*", "cache": {"max_age": 200}},
        {"match": "*.toml", "cache": {"max_age": 100}}
    ]"#
                .to_string(),
            ),
        ]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 7).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;

        let assets_config = dbg!(AssetSourceDirectoryConfiguration::load(&assets_dir))?;
        for f in [
            "index.html",
            "js/index.js",
            "js/index.map.js",
            "css/main.css",
            "css/stylish.css",
        ] {
            assert_eq!(
                assets_config.get_asset_config(assets_dir.join(f).as_path())?,
                AssetConfig {
                    cache: Some(CacheConfig { max_age: Some(999) }),
                    ..Default::default()
                }
            );
        }

        assert_eq!(
            assets_config.get_asset_config(assets_dir.join("nested/the-thing.txt").as_path())?,
            AssetConfig {
                cache: Some(CacheConfig { max_age: Some(400) }),
                ..Default::default()
            },
        );
        assert_eq!(
            assets_config
                .get_asset_config(assets_dir.join("nested/deep/the-next-thing.toml").as_path())?,
            AssetConfig {
                cache: Some(CacheConfig { max_age: Some(100) }),
                ..Default::default()
            },
        );

        Ok(())
    }

    #[test]
    fn json5_config_file_with_comments() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([(
            "".to_string(),
            r#"[
// comment
  {
    "match": "*",
    /*
    look at this beatiful key below, not wrapped in quotes
*/  cache: { max_age: 999 } }
]"#
            .to_string(),
        )]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        assert_eq!(
            assets_config.get_asset_config(assets_dir.join("index.html").as_path())?,
            AssetConfig {
                cache: Some(CacheConfig { max_age: Some(999) }),
                ..Default::default()
            },
        );
        Ok(())
    }

    #[test]
    fn no_content_config_file() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([
            ("".to_string(), "".to_string()),
            ("css".to_string(), "".to_string()),
            ("js".to_string(), "".to_string()),
            ("nested".to_string(), "".to_string()),
            ("nested/deep".to_string(), "".to_string()),
        ]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir);
        assert_eq!(
            assets_config.err().unwrap().to_string(),
            format!(
                "malformed JSON asset config file: {}",
                assets_dir
                    .join(ASSETS_CONFIG_FILENAME_JSON)
                    .to_str()
                    .unwrap()
            )
        );
        Ok(())
    }

    #[test]
    fn invalid_json_config_file() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([("".to_string(), "[[[{{{".to_string())]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir);
        assert_eq!(
            assets_config.err().unwrap().to_string(),
            format!(
                "malformed JSON asset config file: {}",
                assets_dir
                    .join(ASSETS_CONFIG_FILENAME_JSON)
                    .to_str()
                    .unwrap()
            )
        );
        Ok(())
    }

    #[test]
    fn invalid_glob_pattern() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([(
            "".to_string(),
            r#"[
        {"match": "{{{\\\", "cache": {"max_age": 900}},
    ]"#
            .to_string(),
        )]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir);
        assert_eq!(
            assets_config.err().unwrap().to_string(),
            format!(
                "malformed JSON asset config file: {}",
                assets_dir
                    .join(ASSETS_CONFIG_FILENAME_JSON)
                    .to_str()
                    .unwrap()
            )
        );
        Ok(())
    }

    #[test]
    fn invalid_redirect_config() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([(
            "".to_string(),
            r#"[
        {"match": "**/*", "redirect": {"to": {}}},
    ]"#
            .to_string(),
        )]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir);
        assert_eq!(
            assets_config.err().unwrap().to_string(),
            format!(
                "malformed JSON asset config file: {}",
                assets_dir
                    .join(ASSETS_CONFIG_FILENAME_JSON)
                    .to_str()
                    .unwrap()
            )
        );
        Ok(())
    }

    #[test]
    fn invalid_asset_path() -> anyhow::Result<()> {
        let cfg = Some(HashMap::new());
        let assets_temp_dir = create_temporary_assets_directory(cfg, 0).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir)?;
        assert_eq!(
            assets_config.get_asset_config(assets_dir.join("doesnt.exists").as_path())?,
            AssetConfig::default()
        );
        Ok(())
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn no_read_permission() -> anyhow::Result<()> {
        let cfg = Some(HashMap::from([(
            "".to_string(),
            r#"[
        {"match": "*", "cache": {"max_age": 20}}
    ]"#
            .to_string(),
        )]));
        let assets_temp_dir = create_temporary_assets_directory(cfg, 1).unwrap();
        let assets_dir = assets_temp_dir.path().canonicalize()?;
        std::fs::set_permissions(
            assets_dir.join(ASSETS_CONFIG_FILENAME_JSON).as_path(),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        let assets_config = AssetSourceDirectoryConfiguration::load(&assets_dir);
        assert_eq!(
            assets_config.err().unwrap().to_string(),
            format!(
                "unable to read config file: {}",
                assets_dir
                    .join(ASSETS_CONFIG_FILENAME_JSON)
                    .as_path()
                    .to_str()
                    .unwrap()
            )
        );

        Ok(())
    }
}
