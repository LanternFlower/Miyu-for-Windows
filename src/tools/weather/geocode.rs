//! 地名 → 坐标，以及结果缓存。
//!
//! 一个地名可能对应多个地方（同名的县市），`location_score` 按人口和层级挑最
//! 可能的那个。中文地名还要试译名（`translated_location_aliases`），因为
//! Open-Meteo 的索引以拉丁名为主。
//!
//! 缓存有 TTL：地名到坐标几乎不变，但缓存无限增长没意义。

use crate::tools::weather::*;

pub(in crate::tools::weather) const GEOCODING_CACHE_TTL: Duration =
    Duration::from_secs(7 * 24 * 60 * 60);

pub(in crate::tools::weather) const FORECAST_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug)]
pub(in crate::tools::weather) struct CacheEntry<T> {
    pub(in crate::tools::weather) inserted_at: Instant,
    pub(in crate::tools::weather) value: T,
}

pub(in crate::tools::weather) static GEOCODING_CACHE: OnceLock<
    Mutex<HashMap<String, CacheEntry<Vec<GeocodingResult>>>>,
> = OnceLock::new();

pub(in crate::tools::weather) static FORECAST_CACHE: OnceLock<
    Mutex<HashMap<String, CacheEntry<ForecastResponse>>>,
> = OnceLock::new();

pub(in crate::tools::weather) async fn selected_location(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<(Vec<GeocodingResult>, GeocodingResult)> {
    let locations = geocode_location(client, request).await?;
    let selected = select_location(&request.location, &locations)
        .ok_or_else(|| anyhow!("no geocoding results for {}", request.location))?
        .clone();
    Ok((locations, selected))
}

pub(in crate::tools::weather) async fn geocode_location(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<Vec<GeocodingResult>> {
    let cache_key = format!(
        "{}|{}",
        normalize_location(&request.location),
        request.country_code.as_deref().unwrap_or("")
    );
    if let Some(cached) = read_cache(geocoding_cache(), &cache_key, GEOCODING_CACHE_TTL) {
        return Ok(cached);
    }

    let mut results = Vec::new();
    let mut last_error = None;
    for name in geocoding_query_names(&request.location) {
        match fetch_geocoding_results(client, &name, request.country_code.as_deref()).await {
            Ok(mut items) => results.append(&mut items),
            Err(err) => last_error = Some(err),
        }
    }
    dedup_locations(&mut results);
    if results.is_empty() {
        if let Some(err) = last_error {
            return Err(err);
        }
        bail!("no geocoding results for {}", request.location);
    }
    write_cache(
        geocoding_cache(),
        cache_key,
        results.clone(),
        GEOCODING_CACHE_TTL,
    );
    Ok(results)
}

pub(in crate::tools::weather) async fn fetch_geocoding_results(
    client: &reqwest::Client,
    name: &str,
    country_code: Option<&str>,
) -> Result<Vec<GeocodingResult>> {
    let mut query = vec![
        ("name", name),
        ("count", "10"),
        ("language", "zh"),
        ("format", "json"),
    ];
    if let Some(country_code) = country_code {
        query.push(("countryCode", country_code));
    }

    let response: GeocodingResponse = client
        .get(OPEN_METEO_GEOCODING_URL)
        .query(&query)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    Ok(response.results.unwrap_or_default())
}

pub(in crate::tools::weather) fn geocoding_query_names(location: &str) -> Vec<String> {
    let trimmed = location.trim();
    let normalized = normalize_location(trimmed);
    let mut names = vec![trimmed.to_string()];

    for alias in translated_location_aliases(&normalized) {
        names.push((*alias).to_string());
    }

    names.sort();
    names.dedup();
    names
}

pub(in crate::tools::weather) fn translated_location_aliases(
    normalized: &str,
) -> &'static [&'static str] {
    match normalized {
        "东京" | "東亰" | "東京" | "东京都" | "東京都" | "日本东京" | "日本東京" | "日本东京都"
        | "日本東京都" | "东京日本" | "東京日本" => &["Tokyo", "東京"],
        "纽约" | "紐約" | "纽约市" | "紐約市" => &["New York"],
        "伦敦" | "倫敦" => &["London"],
        "巴黎" => &["Paris"],
        "洛杉矶" | "洛杉磯" => &["Los Angeles"],
        "旧金山" | "舊金山" | "三藩市" => &["San Francisco"],
        "首尔" | "首爾" | "汉城" | "漢城" => &["Seoul"],
        "莫斯科" => &["Moscow"],
        "柏林" => &["Berlin"],
        "罗马" | "羅馬" => &["Rome"],
        "曼谷" => &["Bangkok"],
        "新加坡" => &["Singapore"],
        "悉尼" | "雪梨" => &["Sydney"],
        "墨尔本" | "墨爾本" => &["Melbourne"],
        "大阪" | "大阪市" => &["Osaka"],
        "京都" | "京都市" => &["Kyoto"],
        "名古屋" => &["Nagoya"],
        "神户" | "神戶" => &["Kobe"],
        "横滨" | "橫濱" => &["Yokohama"],
        _ => &[],
    }
}

pub(in crate::tools::weather) fn dedup_locations(locations: &mut Vec<GeocodingResult>) {
    let mut seen = std::collections::HashSet::new();
    locations.retain(|location| {
        let key = format!(
            "{}|{}|{:.4}|{:.4}",
            normalize_location(&location.name),
            location.country_code.as_deref().unwrap_or(""),
            location.latitude,
            location.longitude
        );
        seen.insert(key)
    });
}

pub(in crate::tools::weather) fn geocoding_cache(
) -> &'static Mutex<HashMap<String, CacheEntry<Vec<GeocodingResult>>>> {
    GEOCODING_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(in crate::tools::weather) fn forecast_cache(
) -> &'static Mutex<HashMap<String, CacheEntry<ForecastResponse>>> {
    FORECAST_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(in crate::tools::weather) fn read_cache<T: Clone>(
    cache: &Mutex<HashMap<String, CacheEntry<T>>>,
    key: &str,
    ttl: Duration,
) -> Option<T> {
    let cache = cache.lock().ok()?;
    let entry = cache.get(key)?;
    if entry.inserted_at.elapsed() <= ttl {
        Some(entry.value.clone())
    } else {
        None
    }
}

/// 两个缓存各自的条目上限。TTL 只挡得住「同一个地名反复查」，挡不住
/// 「短时间内查一堆不同地名」——那种情况下条目在 TTL 内全是活的。
pub(in crate::tools::weather) const MAX_CACHE_ENTRIES: usize = 512;

pub(in crate::tools::weather) fn write_cache<T>(
    cache: &Mutex<HashMap<String, CacheEntry<T>>>,
    key: String,
    value: T,
    ttl: Duration,
) {
    if let Ok(mut cache) = cache.lock() {
        // 读取只判过期不删;写入时顺手清掉,常驻 daemon 不积死条目。
        cache.retain(|_, entry| entry.inserted_at.elapsed() <= ttl);
        // 清完还超量说明是「短时间内查了成百上千个不同地名」——TTL 拦不住
        // 这种，得有条数上限兜底。淘汰最旧的那批，键是地名/坐标，丢了只是
        // 下次重查一次。
        while cache.len() >= MAX_CACHE_ENTRIES {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.inserted_at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            cache.remove(&oldest);
        }
        cache.insert(
            key,
            CacheEntry {
                inserted_at: Instant::now(),
                value,
            },
        );
    }
}

pub(in crate::tools::weather) fn select_location<'a>(
    query: &str,
    locations: &'a [GeocodingResult],
) -> Option<&'a GeocodingResult> {
    locations
        .iter()
        .max_by_key(|location| location_score(query, location))
}

pub(in crate::tools::weather) fn location_score(query: &str, location: &GeocodingResult) -> i64 {
    let normalized_query = normalize_location(query);
    let normalized_name = normalize_location(&location.name);
    let mut score = 0;
    if normalized_name == normalized_query {
        score += 1_000_000;
    } else if normalized_name.contains(&normalized_query)
        || normalized_query.contains(&normalized_name)
    {
        score += 100_000;
    }
    score += match location.feature_code.as_deref() {
        Some("PPLC") => 80_000,
        Some("PPLA") => 60_000,
        Some("PPLA2") => 50_000,
        Some("PPLA3") => 40_000,
        Some("PPLA4") => 30_000,
        Some("PPL") => 20_000,
        _ => 0,
    };
    score + location.population.unwrap_or(0).min(10_000_000) as i64
}

#[cfg(test)]
mod bounds_tests {
    use super::*;

    /// TTL 只挡「同一个地名反复查」。短时间内查一堆不同地名时，条目全在
    /// TTL 内、一条都不会被 retain 清掉——原来这里没有条数上限。
    #[test]
    fn the_cache_is_bounded_even_when_nothing_has_expired() {
        let cache: Mutex<HashMap<String, CacheEntry<u32>>> = Mutex::new(HashMap::new());
        let ttl = Duration::from_secs(3_600);
        for index in 0..MAX_CACHE_ENTRIES * 3 {
            write_cache(&cache, format!("地名{index}"), index as u32, ttl);
            assert!(
                cache.lock().unwrap().len() <= MAX_CACHE_ENTRIES,
                "第 {index} 次写入后超了上限"
            );
        }
        // 留下的是最近写的那批
        let cache = cache.lock().unwrap();
        assert!(cache.contains_key(&format!("地名{}", MAX_CACHE_ENTRIES * 3 - 1)));
        assert!(!cache.contains_key("地名0"));
    }
}
