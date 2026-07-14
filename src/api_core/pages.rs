use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Copy, Clone, PartialEq, Serialize, Deserialize)]
pub enum OrderBy {
    #[serde(rename = "asc", alias = "ASC")]
    Asc,

    #[default]
    #[serde(rename = "desc", alias = "DESC")]
    Desc,
}

impl OrderBy {
    pub fn reverse(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

impl std::str::FromStr for OrderBy {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            _ => Err(anyhow::anyhow!(
                "invalid orderby: possible values are `asc` or `desc`"
            )),
        }
    }
}

impl std::fmt::Display for OrderBy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Asc => write!(f, "asc"),
            Self::Desc => write!(f, "desc"),
        }
    }
}

#[derive(Default, Clone, Serialize, Deserialize, Debug)]
pub struct ListResponseMeta {
    pub page: u32,
    pub limit: u32,
    pub offset: u32,
    pub has_more: bool,
    pub total_records: u64,
}

impl ListResponseMeta {
    pub fn new(limit: u32, offset: u32, total: u64) -> Self {
        Self {
            page: (offset / limit.max(1)),
            limit,
            offset,
            has_more: u64::from(offset + limit) < total,
            total_records: total,
        }
    }
}

#[test]
fn test_page_is_correct() {
    let meta = ListResponseMeta::new(10, 0, 320);
    assert_eq!(meta.limit, 10);
    assert_eq!(meta.offset, 0);
    assert_eq!(meta.page, 0);
    assert_eq!(meta.total_records, 320);
    assert!(meta.has_more);

    let meta = ListResponseMeta::new(10, 10, 320);
    assert_eq!(meta.page, 1);

    let meta = ListResponseMeta::new(10, 14, 320);
    assert_eq!(meta.page, 1);

    let meta = ListResponseMeta::new(10, 20, 320);
    assert_eq!(meta.page, 2);

    let meta = ListResponseMeta::new(10, 220, 320);
    assert_eq!(meta.page, 22);

    let meta = ListResponseMeta::new(0, 0, 320);
    assert_eq!(meta.page, 0);
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct ListResult<T: Serialize> {
    pub meta: Option<ListResponseMeta>,
    pub records: Vec<T>,
}

impl<T: Serialize> From<Vec<T>> for ListResult<T> {
    fn from(val: Vec<T>) -> Self {
        ListResult {
            records: val,
            meta: None,
        }
    }
}

#[derive(Clone, Default, Debug, Deserialize, Serialize)]
pub struct PageParams {
    #[serde(default)]
    pub order: OrderBy,
    #[serde(default, deserialize_with = "number_or_string")]
    pub limit: Option<u32>,
    #[serde(default, deserialize_with = "number_or_string")]
    pub offset: Option<u32>,
    #[serde(default, deserialize_with = "number_or_string")]
    pub page: Option<u32>,
}

fn number_or_string<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize, Debug)]
    #[serde(untagged)]
    pub enum Value {
        Numeric(u32),
        Textual(String),
    }

    let val = Option::<Value>::deserialize(deserializer)?;
    let Some(v) = val else {
        return Ok(None);
    };

    match v {
        Value::Numeric(n) => Ok(Some(n)),
        Value::Textual(s) => Ok(Some(s.parse::<u32>().map_err(serde::de::Error::custom)?)),
    }
}

impl PageParams {
    pub fn limit_offset(&self) -> anyhow::Result<(u32, u32)> {
        const DEFAULT_LIMIT: u32 = 50;
        const MAX_LIMIT: u32 = 1000;
        const MAX_PAGE: u32 = 100;

        let Some(limit) = self.limit else {
            // you can't set page
            // or offset without limit!
            return Ok((50, 0));
        };

        let limit = if limit == 0 { DEFAULT_LIMIT } else { limit };

        if limit > MAX_LIMIT {
            anyhow::bail!("limit({limit}) more than max allowed({MAX_LIMIT})")
        }

        let offset = self.offset.unwrap_or_default();
        if offset > MAX_PAGE * limit {
            anyhow::bail!(
                "offset({offset}) more than max allowed({})",
                MAX_PAGE * limit
            )
        }
        if offset > 0 {
            return Ok((limit, offset));
        }

        // manual offset has higher priority
        // than page parameter
        let page = self.page.unwrap_or_default();
        if page > MAX_PAGE {
            anyhow::bail!("page({page}) more than max allowed({MAX_PAGE})")
        }

        Ok((limit, page * limit))
    }
}
