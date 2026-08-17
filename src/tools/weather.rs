use super::{ToolRegistry, ToolSpec};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const TOOL_NAME: &str = "get_weather";
const TOOL_DESC: &str = "气象数据查询。query_type 默认 forecast，支持 forecast 当前天气和预报、air_quality 空气质量、historical 历史天气、marine 海洋天气、climate 气候趋势、elevation 海拔。location 可传城市、地点、邮编或机场码；仅 forecast 支持空字符串自动定位并 fallback 到 wttr.in。days 可选，默认 3。start_date/end_date 用于 historical 和 climate。country_code 可选，用 ISO-3166-1 alpha2 国家代码消除重名地点歧义，例如 CN、JP、US。";
const OPEN_METEO_GEOCODING_URL: &str = "https://geocoding-api.open-meteo.com/v1/search";
const OPEN_METEO_FORECAST_URL: &str = "https://api.open-meteo.com/v1/forecast";
const OPEN_METEO_AIR_QUALITY_URL: &str = "https://air-quality-api.open-meteo.com/v1/air-quality";
const OPEN_METEO_ARCHIVE_URL: &str = "https://archive-api.open-meteo.com/v1/archive";
const OPEN_METEO_MARINE_URL: &str = "https://marine-api.open-meteo.com/v1/marine";
const OPEN_METEO_CLIMATE_URL: &str = "https://climate-api.open-meteo.com/v1/climate";
const OPEN_METEO_ELEVATION_URL: &str = "https://api.open-meteo.com/v1/elevation";
const GEOCODING_CACHE_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const FORECAST_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

pub fn register(registry: &mut ToolRegistry) {
    registry.register(ToolSpec::new(
        TOOL_NAME,
        TOOL_DESC,
        json!({
            "type": "object",
            "properties": {
                "location": { "type": "string", "description": "城市、地点、邮编或机场码；空字符串表示自动定位。" },
                "query_type": { "type": "string", "enum": ["forecast", "air_quality", "historical", "marine", "climate", "elevation"], "description": "查询类型。forecast=当前天气和预报，air_quality=空气质量，historical=历史天气，marine=海洋天气，climate=气候趋势，elevation=海拔；默认 forecast。" },
                "days": { "type": "integer", "description": "返回预报天数，默认 3，最大 7。" },
                "start_date": { "type": "string", "description": "开始日期，格式 YYYY-MM-DD。用于 historical 和 climate。" },
                "end_date": { "type": "string", "description": "结束日期，格式 YYYY-MM-DD。用于 historical 和 climate；省略时等于 start_date。" },
                "country_code": { "type": "string", "description": "可选 ISO-3166-1 alpha2 国家代码，用于消除重名地点歧义，例如 CN、JP、US。" }
            },
            "additionalProperties": false
        }),
        |args| async move { get_weather(args).await },
    ));
}

async fn get_weather(args: Value) -> Result<String> {
    let request = WeatherRequest::from_args(&args);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("miyu-weather/0.1")
        .build()?;

    if request.location.is_empty() && request.query_type == WeatherQueryType::Forecast {
        return get_weather_wttr(&client, "", "auto_location").await;
    }

    if request.location.is_empty() {
        bail!(
            "location is required for {} query",
            request.query_type.as_str()
        );
    }

    match request.query_type {
        WeatherQueryType::Forecast => match get_weather_open_meteo(&client, &request).await {
            Ok(weather) => Ok(weather),
            Err(open_meteo_error) => {
                get_weather_wttr(&client, &request.location, "open_meteo_fallback")
                    .await
                    .map_err(|wttr_error| {
                        anyhow!(
                    "weather query failed; open_meteo: {open_meteo_error}; wttr.in: {wttr_error}"
                )
                    })
            }
        },
        WeatherQueryType::AirQuality => get_air_quality_open_meteo(&client, &request).await,
        WeatherQueryType::Historical => get_historical_open_meteo(&client, &request).await,
        WeatherQueryType::Marine => get_marine_open_meteo(&client, &request).await,
        WeatherQueryType::Climate => get_climate_open_meteo(&client, &request).await,
        WeatherQueryType::Elevation => get_elevation_open_meteo(&client, &request).await,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum WeatherQueryType {
    Forecast,
    AirQuality,
    Historical,
    Marine,
    Climate,
    Elevation,
}

impl WeatherQueryType {
    fn from_args(args: &Value) -> Self {
        match args
            .get("query_type")
            .and_then(Value::as_str)
            .unwrap_or("forecast")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "air_quality" | "air-quality" | "air" | "aqi" => Self::AirQuality,
            "historical" | "history" | "archive" => Self::Historical,
            "marine" | "sea" | "ocean" => Self::Marine,
            "climate" | "climate_change" | "climate-change" => Self::Climate,
            "elevation" | "altitude" => Self::Elevation,
            _ => Self::Forecast,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Forecast => "forecast",
            Self::AirQuality => "air_quality",
            Self::Historical => "historical",
            Self::Marine => "marine",
            Self::Climate => "climate",
            Self::Elevation => "elevation",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WeatherRequest {
    location: String,
    query_type: WeatherQueryType,
    days: usize,
    start_date: Option<String>,
    end_date: Option<String>,
    country_code: Option<String>,
}

impl WeatherRequest {
    fn from_args(args: &Value) -> Self {
        let location = args
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let days = args
            .get("days")
            .and_then(Value::as_u64)
            .unwrap_or(3)
            .clamp(1, 7) as usize;
        let start_date = optional_trimmed_string(args, "start_date");
        let end_date = optional_trimmed_string(args, "end_date");
        let country_code = args
            .get("country_code")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_uppercase());

        Self {
            location,
            query_type: WeatherQueryType::from_args(args),
            days,
            start_date,
            end_date,
            country_code,
        }
    }
}

fn optional_trimmed_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[derive(Clone, Debug, Deserialize)]
struct GeocodingResponse {
    results: Option<Vec<GeocodingResult>>,
}

#[derive(Clone, Debug, Deserialize)]
struct GeocodingResult {
    name: String,
    latitude: f64,
    longitude: f64,
    elevation: Option<f64>,
    feature_code: Option<String>,
    country_code: Option<String>,
    country: Option<String>,
    admin1: Option<String>,
    admin2: Option<String>,
    timezone: Option<String>,
    population: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct ForecastResponse {
    current: CurrentWeather,
    daily: DailyWeather,
}

#[derive(Clone, Debug, Deserialize)]
struct CurrentWeather {
    time: String,
    temperature_2m: Option<f64>,
    relative_humidity_2m: Option<u64>,
    apparent_temperature: Option<f64>,
    is_day: Option<u8>,
    precipitation: Option<f64>,
    weather_code: Option<i64>,
    cloud_cover: Option<u64>,
    wind_speed_10m: Option<f64>,
    wind_direction_10m: Option<f64>,
    wind_gusts_10m: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct DailyWeather {
    time: Vec<String>,
    weather_code: Option<Vec<i64>>,
    temperature_2m_max: Option<Vec<f64>>,
    temperature_2m_min: Option<Vec<f64>>,
    precipitation_sum: Option<Vec<f64>>,
    precipitation_probability_max: Option<Vec<u64>>,
    wind_speed_10m_max: Option<Vec<f64>>,
    wind_gusts_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
struct AirQualityResponse {
    current: AirQualityCurrent,
}

#[derive(Clone, Debug, Deserialize)]
struct AirQualityCurrent {
    time: String,
    european_aqi: Option<u64>,
    us_aqi: Option<u64>,
    pm10: Option<f64>,
    pm2_5: Option<f64>,
    carbon_monoxide: Option<f64>,
    nitrogen_dioxide: Option<f64>,
    sulphur_dioxide: Option<f64>,
    ozone: Option<f64>,
    uv_index: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalResponse {
    daily: HistoricalDaily,
}

#[derive(Clone, Debug, Deserialize)]
struct HistoricalDaily {
    time: Vec<String>,
    weather_code: Option<Vec<i64>>,
    temperature_2m_max: Option<Vec<f64>>,
    temperature_2m_min: Option<Vec<f64>>,
    precipitation_sum: Option<Vec<f64>>,
    wind_speed_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
struct MarineResponse {
    current: MarineCurrent,
    daily: MarineDaily,
}

#[derive(Clone, Debug, Deserialize)]
struct MarineCurrent {
    time: String,
    wave_height: Option<f64>,
    wave_direction: Option<f64>,
    wave_period: Option<f64>,
    sea_surface_temperature: Option<f64>,
    ocean_current_velocity: Option<f64>,
    ocean_current_direction: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
struct MarineDaily {
    time: Vec<String>,
    wave_height_max: Option<Vec<f64>>,
    wave_direction_dominant: Option<Vec<f64>>,
    wave_period_max: Option<Vec<f64>>,
    swell_wave_height_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ClimateResponse {
    daily: ClimateDaily,
}

#[derive(Clone, Debug, Deserialize)]
struct ClimateDaily {
    time: Vec<String>,
    temperature_2m_mean: Option<Vec<f64>>,
    temperature_2m_max: Option<Vec<f64>>,
    temperature_2m_min: Option<Vec<f64>>,
    precipitation_sum: Option<Vec<f64>>,
    wind_speed_10m_max: Option<Vec<f64>>,
}

#[derive(Clone, Debug, Deserialize)]
struct ElevationResponse {
    elevation: Vec<Option<f64>>,
}

#[derive(Clone, Debug)]
struct CacheEntry<T> {
    inserted_at: Instant,
    value: T,
}

static GEOCODING_CACHE: OnceLock<Mutex<HashMap<String, CacheEntry<Vec<GeocodingResult>>>>> =
    OnceLock::new();
static FORECAST_CACHE: OnceLock<Mutex<HashMap<String, CacheEntry<ForecastResponse>>>> =
    OnceLock::new();

async fn get_weather_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let locations = geocode_location(client, request).await?;
    let selected = select_location(&request.location, &locations)
        .ok_or_else(|| anyhow!("no geocoding results for {}", request.location))?;
    let forecast = fetch_forecast(client, selected, request.days).await?;
    format_open_meteo_result(selected, &locations, &forecast)
}

async fn selected_location(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<(Vec<GeocodingResult>, GeocodingResult)> {
    let locations = geocode_location(client, request).await?;
    let selected = select_location(&request.location, &locations)
        .ok_or_else(|| anyhow!("no geocoding results for {}", request.location))?
        .clone();
    Ok((locations, selected))
}

async fn get_air_quality_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let days = request.days.min(7).to_string();
    let response: AirQualityResponse = client
        .get(OPEN_METEO_AIR_QUALITY_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "european_aqi,us_aqi,pm10,pm2_5,carbon_monoxide,nitrogen_dioxide,sulphur_dioxide,ozone,uv_index",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_air_quality_result(&selected, &locations, &response)
}

async fn get_historical_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let start_date = request
        .start_date
        .as_deref()
        .ok_or_else(|| anyhow!("historical query requires start_date"))?;
    let end_date = request.end_date.as_deref().unwrap_or(start_date);
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: HistoricalResponse = client
        .get(OPEN_METEO_ARCHIVE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            ("start_date", start_date),
            ("end_date", end_date),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,wind_speed_10m_max",
            ),
            ("timezone", "auto"),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_historical_result(&selected, &locations, &response, start_date, end_date)
}

async fn get_marine_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let days = request.days.min(7).to_string();
    let response: MarineResponse = client
        .get(OPEN_METEO_MARINE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "wave_height,wave_direction,wave_period,sea_surface_temperature,ocean_current_velocity,ocean_current_direction",
            ),
            (
                "daily",
                "wave_height_max,wave_direction_dominant,wave_period_max,swell_wave_height_max",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
            ("length_unit", "metric"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_marine_result(&selected, &locations, &response)
}

async fn get_climate_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let start_date = request
        .start_date
        .as_deref()
        .ok_or_else(|| anyhow!("climate query requires start_date"))?;
    let end_date = request.end_date.as_deref().unwrap_or(start_date);
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: ClimateResponse = client
        .get(OPEN_METEO_CLIMATE_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            ("start_date", start_date),
            ("end_date", end_date),
            ("models", "EC_Earth3P_HR"),
            (
                "daily",
                "temperature_2m_mean,temperature_2m_max,temperature_2m_min,precipitation_sum,wind_speed_10m_max",
            ),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_climate_result(&selected, &locations, &response, start_date, end_date)
}

async fn get_elevation_open_meteo(
    client: &reqwest::Client,
    request: &WeatherRequest,
) -> Result<String> {
    let (locations, selected) = selected_location(client, request).await?;
    let latitude = selected.latitude.to_string();
    let longitude = selected.longitude.to_string();
    let response: ElevationResponse = client
        .get(OPEN_METEO_ELEVATION_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    format_elevation_result(&selected, &locations, &response)
}

async fn geocode_location(
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
    write_cache(geocoding_cache(), cache_key, results.clone(), GEOCODING_CACHE_TTL);
    Ok(results)
}

async fn fetch_geocoding_results(
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

fn geocoding_query_names(location: &str) -> Vec<String> {
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

fn translated_location_aliases(normalized: &str) -> &'static [&'static str] {
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

fn dedup_locations(locations: &mut Vec<GeocodingResult>) {
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

async fn fetch_forecast(
    client: &reqwest::Client,
    location: &GeocodingResult,
    days: usize,
) -> Result<ForecastResponse> {
    let cache_key = format!(
        "{:.3}|{:.3}|{}",
        location.latitude, location.longitude, days
    );
    if let Some(cached) = read_cache(forecast_cache(), &cache_key, FORECAST_CACHE_TTL) {
        return Ok(cached);
    }

    let latitude = location.latitude.to_string();
    let longitude = location.longitude.to_string();
    let days = days.to_string();
    let response: ForecastResponse = client
        .get(OPEN_METEO_FORECAST_URL)
        .query(&[
            ("latitude", latitude.as_str()),
            ("longitude", longitude.as_str()),
            (
                "current",
                "temperature_2m,relative_humidity_2m,apparent_temperature,is_day,precipitation,weather_code,cloud_cover,wind_speed_10m,wind_direction_10m,wind_gusts_10m",
            ),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max,wind_gusts_10m_max",
            ),
            ("forecast_days", days.as_str()),
            ("timezone", "auto"),
            ("temperature_unit", "celsius"),
            ("wind_speed_unit", "kmh"),
            ("precipitation_unit", "mm"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    write_cache(forecast_cache(), cache_key, response.clone(), FORECAST_CACHE_TTL);
    Ok(response)
}

fn geocoding_cache() -> &'static Mutex<HashMap<String, CacheEntry<Vec<GeocodingResult>>>> {
    GEOCODING_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn forecast_cache() -> &'static Mutex<HashMap<String, CacheEntry<ForecastResponse>>> {
    FORECAST_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn read_cache<T: Clone>(
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

fn write_cache<T>(cache: &Mutex<HashMap<String, CacheEntry<T>>>, key: String, value: T, ttl: Duration) {
    if let Ok(mut cache) = cache.lock() {
        // 读取只判过期不删;写入时顺手清掉,常驻 daemon 不积死条目。
        cache.retain(|_, entry| entry.inserted_at.elapsed() <= ttl);
        cache.insert(
            key,
            CacheEntry {
                inserted_at: Instant::now(),
                value,
            },
        );
    }
}

fn select_location<'a>(
    query: &str,
    locations: &'a [GeocodingResult],
) -> Option<&'a GeocodingResult> {
    locations
        .iter()
        .max_by_key(|location| location_score(query, location))
}

fn location_score(query: &str, location: &GeocodingResult) -> i64 {
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

fn format_open_meteo_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    forecast: &ForecastResponse,
) -> Result<String> {
    let current = &forecast.current;
    let current_condition = current
        .weather_code
        .map(weather_code_label)
        .unwrap_or("未知天气");
    let place = format_location(location);
    let summary = format!(
        "{}：当前{}，{}，体感{}，湿度{}，{}{}，阵风{}。{}",
        place,
        current_condition,
        format_temperature(current.temperature_2m),
        format_temperature(current.apparent_temperature),
        format_percent(current.relative_humidity_2m),
        current
            .wind_direction_10m
            .map(wind_direction_label)
            .unwrap_or("未知风向"),
        format_speed(current.wind_speed_10m),
        format_speed(current.wind_gusts_10m),
        format_today(&forecast.daily)
    );

    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "resolved_location": {
            "name": location.name,
            "admin1": location.admin1,
            "admin2": location.admin2,
            "country": location.country,
            "country_code": location.country_code,
            "timezone": location.timezone,
            "latitude": location.latitude,
            "longitude": location.longitude,
            "elevation_m": location.elevation,
        },
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "summary": summary,
        "current": {
            "time": current.time,
            "condition": current_condition,
            "is_day": current.is_day,
            "temperature_c": current.temperature_2m,
            "apparent_temperature_c": current.apparent_temperature,
            "humidity_percent": current.relative_humidity_2m,
            "precipitation_mm": current.precipitation,
            "cloud_cover_percent": current.cloud_cover,
            "wind_speed_kmh": current.wind_speed_10m,
            "wind_direction_degrees": current.wind_direction_10m,
            "wind_direction": current.wind_direction_10m.map(wind_direction_label),
            "wind_gusts_kmh": current.wind_gusts_10m,
        },
        "daily": daily_json(&forecast.daily),
        "source": {
            "name": "Open-Meteo",
            "attribution": "Weather data by Open-Meteo.com (https://open-meteo.com/)"
        }
    }))?)
}

fn location_json(location: &GeocodingResult) -> Value {
    json!({
        "name": location.name,
        "admin1": location.admin1,
        "admin2": location.admin2,
        "country": location.country,
        "country_code": location.country_code,
        "timezone": location.timezone,
        "latitude": location.latitude,
        "longitude": location.longitude,
        "population": location.population,
    })
}

fn daily_json(daily: &DailyWeather) -> Vec<Value> {
    daily
        .time
        .iter()
        .enumerate()
        .map(|(index, date)| {
            let code = value_at(daily.weather_code.as_deref(), index);
            json!({
                "date": date,
                "condition": code.map(weather_code_label),
                "weather_code": code,
                "temperature_min_c": value_at(daily.temperature_2m_min.as_deref(), index),
                "temperature_max_c": value_at(daily.temperature_2m_max.as_deref(), index),
                "precipitation_sum_mm": value_at(daily.precipitation_sum.as_deref(), index),
                "precipitation_probability_max_percent": value_at(daily.precipitation_probability_max.as_deref(), index),
                "wind_speed_max_kmh": value_at(daily.wind_speed_10m_max.as_deref(), index),
                "wind_gusts_max_kmh": value_at(daily.wind_gusts_10m_max.as_deref(), index),
            })
        })
        .collect()
}

fn format_air_quality_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    response: &AirQualityResponse,
) -> Result<String> {
    let current = &response.current;
    let place = format_location(location);
    let summary =
        format!(
        "{}：当前空气质量 EU AQI {}({})，US AQI {}({})，PM2.5 {}，PM10 {}，臭氧{}，NO2 {}，UV {}。",
        place,
        format_count(current.european_aqi),
        current.european_aqi.map(european_aqi_label).unwrap_or("未知"),
        format_count(current.us_aqi),
        current.us_aqi.map(us_aqi_label).unwrap_or("未知"),
        format_micrograms(current.pm2_5),
        format_micrograms(current.pm10),
        format_micrograms(current.ozone),
        format_micrograms(current.nitrogen_dioxide),
        format_optional_number(current.uv_index),
    );
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "query_type": "air_quality",
        "resolved_location": location_json(location),
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "summary": summary,
        "current": {
            "time": current.time,
            "european_aqi": current.european_aqi,
            "european_aqi_label": current.european_aqi.map(european_aqi_label),
            "us_aqi": current.us_aqi,
            "us_aqi_label": current.us_aqi.map(us_aqi_label),
            "pm10_ug_m3": current.pm10,
            "pm2_5_ug_m3": current.pm2_5,
            "carbon_monoxide_ug_m3": current.carbon_monoxide,
            "nitrogen_dioxide_ug_m3": current.nitrogen_dioxide,
            "sulphur_dioxide_ug_m3": current.sulphur_dioxide,
            "ozone_ug_m3": current.ozone,
            "uv_index": current.uv_index,
        },
        "source": open_meteo_source("空气质量数据包含 CAMS 来源，请在面向用户展示时保留来源说明。")
    }))?)
}

fn format_historical_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    response: &HistoricalResponse,
    start_date: &str,
    end_date: &str,
) -> Result<String> {
    let place = format_location(location);
    let first_date = response
        .daily
        .time
        .first()
        .map(String::as_str)
        .unwrap_or(start_date);
    let first_condition = value_at(response.daily.weather_code.as_deref(), 0)
        .map(weather_code_label)
        .unwrap_or("未知天气");
    let summary = format!(
        "{}：历史天气 {} 至 {}，首日({}){}，{}-{}，降水{}，最大风速{}。",
        place,
        start_date,
        end_date,
        first_date,
        first_condition,
        format_temperature(value_at(response.daily.temperature_2m_min.as_deref(), 0)),
        format_temperature(value_at(response.daily.temperature_2m_max.as_deref(), 0)),
        format_precipitation(value_at(response.daily.precipitation_sum.as_deref(), 0)),
        format_speed(value_at(response.daily.wind_speed_10m_max.as_deref(), 0)),
    );
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "query_type": "historical",
        "resolved_location": location_json(location),
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "start_date": start_date,
        "end_date": end_date,
        "summary": summary,
        "daily": historical_daily_json(&response.daily),
        "source": open_meteo_source("历史天气基于再分析数据，不等同于站点实测。")
    }))?)
}

fn historical_daily_json(daily: &HistoricalDaily) -> Vec<Value> {
    daily
        .time
        .iter()
        .enumerate()
        .map(|(index, date)| {
            let code = value_at(daily.weather_code.as_deref(), index);
            json!({
                "date": date,
                "condition": code.map(weather_code_label),
                "weather_code": code,
                "temperature_min_c": value_at(daily.temperature_2m_min.as_deref(), index),
                "temperature_max_c": value_at(daily.temperature_2m_max.as_deref(), index),
                "precipitation_sum_mm": value_at(daily.precipitation_sum.as_deref(), index),
                "wind_speed_max_kmh": value_at(daily.wind_speed_10m_max.as_deref(), index),
            })
        })
        .collect()
}

fn format_marine_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    response: &MarineResponse,
) -> Result<String> {
    let current = &response.current;
    let place = format_location(location);
    let summary = format!(
        "{}：当前海况，浪高{}，浪向{}，周期{}，海表温度{}，洋流{}，流向{}。",
        place,
        format_meters(current.wave_height),
        current
            .wave_direction
            .map(wind_direction_label)
            .unwrap_or("未知"),
        format_seconds(current.wave_period),
        format_temperature(current.sea_surface_temperature),
        format_speed(current.ocean_current_velocity),
        current
            .ocean_current_direction
            .map(wind_direction_label)
            .unwrap_or("未知"),
    );
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "query_type": "marine",
        "resolved_location": location_json(location),
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "summary": summary,
        "current": {
            "time": current.time,
            "wave_height_m": current.wave_height,
            "wave_direction_degrees": current.wave_direction,
            "wave_direction": current.wave_direction.map(wind_direction_label),
            "wave_period_seconds": current.wave_period,
            "sea_surface_temperature_c": current.sea_surface_temperature,
            "ocean_current_velocity_kmh": current.ocean_current_velocity,
            "ocean_current_direction_degrees": current.ocean_current_direction,
            "ocean_current_direction": current.ocean_current_direction.map(wind_direction_label),
        },
        "daily": marine_daily_json(&response.daily),
        "source": open_meteo_source("海洋数据不适合沿岸导航，不能替代航海图书或官方海事预报。")
    }))?)
}

fn marine_daily_json(daily: &MarineDaily) -> Vec<Value> {
    daily
        .time
        .iter()
        .enumerate()
        .map(|(index, date)| {
            json!({
                "date": date,
                "wave_height_max_m": value_at(daily.wave_height_max.as_deref(), index),
                "wave_direction_dominant_degrees": value_at(daily.wave_direction_dominant.as_deref(), index),
                "wave_period_max_seconds": value_at(daily.wave_period_max.as_deref(), index),
                "swell_wave_height_max_m": value_at(daily.swell_wave_height_max.as_deref(), index),
            })
        })
        .collect()
}

fn format_climate_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    response: &ClimateResponse,
    start_date: &str,
    end_date: &str,
) -> Result<String> {
    let place = format_location(location);
    let first_date = response
        .daily
        .time
        .first()
        .map(String::as_str)
        .unwrap_or(start_date);
    let summary = format!(
        "{}：气候趋势 {} 至 {}，首日({})平均温度{}，{}-{}，降水{}，最大风速{}。",
        place,
        start_date,
        end_date,
        first_date,
        format_temperature(value_at(response.daily.temperature_2m_mean.as_deref(), 0)),
        format_temperature(value_at(response.daily.temperature_2m_min.as_deref(), 0)),
        format_temperature(value_at(response.daily.temperature_2m_max.as_deref(), 0)),
        format_precipitation(value_at(response.daily.precipitation_sum.as_deref(), 0)),
        format_speed(value_at(response.daily.wind_speed_10m_max.as_deref(), 0)),
    );
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "query_type": "climate",
        "resolved_location": location_json(location),
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "start_date": start_date,
        "end_date": end_date,
        "model": "EC_Earth3P_HR",
        "summary": summary,
        "daily": climate_daily_json(&response.daily),
        "source": open_meteo_source("气候数据是单个 CMIP6 高分辨率气候模型的趋势数据，不等同于实际天气观测；长期判断应比较多个模型。")
    }))?)
}

fn climate_daily_json(daily: &ClimateDaily) -> Vec<Value> {
    daily
        .time
        .iter()
        .enumerate()
        .map(|(index, date)| {
            json!({
                "date": date,
                "temperature_mean_c": value_at(daily.temperature_2m_mean.as_deref(), index),
                "temperature_min_c": value_at(daily.temperature_2m_min.as_deref(), index),
                "temperature_max_c": value_at(daily.temperature_2m_max.as_deref(), index),
                "precipitation_sum_mm": value_at(daily.precipitation_sum.as_deref(), index),
                "wind_speed_max_kmh": value_at(daily.wind_speed_10m_max.as_deref(), index),
            })
        })
        .collect()
}

fn format_elevation_result(
    location: &GeocodingResult,
    alternatives: &[GeocodingResult],
    response: &ElevationResponse,
) -> Result<String> {
    let elevation = response
        .elevation
        .first()
        .copied()
        .flatten()
        .or(location.elevation);
    let place = format_location(location);
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "open_meteo",
        "query_type": "elevation",
        "resolved_location": location_json(location),
        "alternatives": alternatives.iter().skip(1).map(location_json).collect::<Vec<_>>(),
        "summary": format!("{}：海拔{}。", place, elevation.map(|value| format!("{value:.0} 米")).unwrap_or_else(|| "未知".to_string())),
        "elevation_m": elevation,
        "source": open_meteo_source("海拔数据来自 90 米分辨率数字高程模型。")
    }))?)
}

fn value_at<T: Copy>(values: Option<&[T]>, index: usize) -> Option<T> {
    values.and_then(|values| values.get(index).copied())
}

fn format_today(daily: &DailyWeather) -> String {
    let Some(date) = daily.time.first() else {
        return "今日预报暂无。".to_string();
    };
    let condition = value_at(daily.weather_code.as_deref(), 0)
        .map(weather_code_label)
        .unwrap_or("未知天气");
    format!(
        "今日({date}){}，{}-{}，降水概率{}，降水{}，最大风速{}。",
        condition,
        format_temperature(value_at(daily.temperature_2m_min.as_deref(), 0)),
        format_temperature(value_at(daily.temperature_2m_max.as_deref(), 0)),
        format_percent(value_at(daily.precipitation_probability_max.as_deref(), 0)),
        format_precipitation(value_at(daily.precipitation_sum.as_deref(), 0)),
        format_speed(value_at(daily.wind_speed_10m_max.as_deref(), 0)),
    )
}

async fn get_weather_wttr(
    client: &reqwest::Client,
    location: &str,
    fallback_reason: &str,
) -> Result<String> {
    let path = if location.is_empty() {
        String::new()
    } else {
        format!("/{}", urlencoding::encode(location))
    };
    let url = format!("https://wttr.in{path}?format=%C+%t+%w+%l");
    let text = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let text = text.trim();
    if text.is_empty() {
        bail!("weather response was empty");
    }
    Ok(serde_json::to_string_pretty(&json!({
        "provider": "wttr_in",
        "mode": "fallback",
        "fallback_reason": fallback_reason,
        "summary": format!("current weather(condition,temperature,wind,location): {text}"),
        "source": {
            "name": "wttr.in"
        }
    }))?)
}

fn format_location(location: &GeocodingResult) -> String {
    [
        Some(location.name.as_str()),
        location.admin1.as_deref(),
        location.country.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .collect::<Vec<_>>()
    .join("，")
}

fn normalize_location(value: &str) -> String {
    value.trim().to_lowercase().replace(char::is_whitespace, "")
}

fn format_temperature(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}°C"))
        .unwrap_or_else(|| "未知温度".to_string())
}

fn format_percent(value: Option<u64>) -> String {
    value
        .map(|value| format!("{value}%"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_speed(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} km/h"))
        .unwrap_or_else(|| "未知风速".to_string())
}

fn format_precipitation(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} mm"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_micrograms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} µg/m³"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_optional_number(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_count(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "未知".to_string())
}

fn format_meters(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} m"))
        .unwrap_or_else(|| "未知".to_string())
}

fn format_seconds(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.1} s"))
        .unwrap_or_else(|| "未知".to_string())
}

fn european_aqi_label(value: u64) -> &'static str {
    match value {
        0..=20 => "良好",
        21..=40 => "尚可",
        41..=60 => "中等",
        61..=80 => "差",
        81..=100 => "很差",
        _ => "极差",
    }
}

fn us_aqi_label(value: u64) -> &'static str {
    match value {
        0..=50 => "良好",
        51..=100 => "中等",
        101..=150 => "对敏感人群不健康",
        151..=200 => "不健康",
        201..=300 => "很不健康",
        _ => "危险",
    }
}

fn open_meteo_source(note: &str) -> Value {
    json!({
        "name": "Open-Meteo",
        "attribution": "Weather data by Open-Meteo.com (https://open-meteo.com/)",
        "note": note,
    })
}

fn weather_code_label(code: i64) -> &'static str {
    match code {
        0 => "晴",
        1 => "大部晴朗",
        2 => "局部多云",
        3 => "阴",
        45 => "雾",
        48 => "冻雾",
        51 => "小毛毛雨",
        53 => "中等毛毛雨",
        55 => "大毛毛雨",
        56 => "小冻毛毛雨",
        57 => "大冻毛毛雨",
        61 => "小雨",
        63 => "中雨",
        65 => "大雨",
        66 => "小冻雨",
        67 => "大冻雨",
        71 => "小雪",
        73 => "中雪",
        75 => "大雪",
        77 => "雪粒",
        80 => "小阵雨",
        81 => "中等阵雨",
        82 => "强阵雨",
        85 => "小阵雪",
        86 => "大阵雪",
        95 => "雷暴",
        96 => "雷暴伴小冰雹",
        99 => "雷暴伴大冰雹",
        _ => "未知天气",
    }
}

fn wind_direction_label(degrees: f64) -> &'static str {
    let normalized = degrees.rem_euclid(360.0);
    let index = ((normalized + 22.5) / 45.0).floor() as usize % 8;
    match index {
        0 => "北风",
        1 => "东北风",
        2 => "东风",
        3 => "东南风",
        4 => "南风",
        5 => "西南风",
        6 => "西风",
        _ => "西北风",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归(PR#31):读取只判过期不删,缓存无界增长;写入时应清扫。
    #[test]
    fn write_cache_sweeps_expired_entries() {
        let cache: Mutex<HashMap<String, CacheEntry<u32>>> = Mutex::new(HashMap::new());
        write_cache(&cache, "old".to_string(), 1, Duration::from_secs(3600));
        std::thread::sleep(Duration::from_millis(5));
        // 用 1ns TTL 触发清扫:5ms 前写入的 old 必然过期,new 本身刚插入。
        write_cache(&cache, "new".to_string(), 2, Duration::from_nanos(1));
        let cache = cache.lock().unwrap();
        assert!(!cache.contains_key("old"));
        assert!(cache.contains_key("new"));
    }

    #[test]
    fn parses_args_with_defaults_and_clamps_days() {
        let request = WeatherRequest::from_args(&json!({
            "location": " Beijing ",
            "query_type": "air_quality",
            "days": 99,
            "start_date": "2024-01-01",
            "country_code": "cn"
        }));
        assert_eq!(request.location, "Beijing");
        assert_eq!(request.query_type, WeatherQueryType::AirQuality);
        assert_eq!(request.days, 7);
        assert_eq!(request.start_date.as_deref(), Some("2024-01-01"));
        assert_eq!(request.country_code.as_deref(), Some("CN"));

        let request = WeatherRequest::from_args(&json!({}));
        assert_eq!(request.location, "");
        assert_eq!(request.query_type, WeatherQueryType::Forecast);
        assert_eq!(request.days, 3);
        assert_eq!(request.country_code, None);

        let request = WeatherRequest::from_args(&json!({"query_type": "climate"}));
        assert_eq!(request.query_type, WeatherQueryType::Climate);
    }

    #[test]
    fn maps_weather_codes_to_chinese_labels() {
        assert_eq!(weather_code_label(0), "晴");
        assert_eq!(weather_code_label(3), "阴");
        assert_eq!(weather_code_label(53), "中等毛毛雨");
        assert_eq!(weather_code_label(65), "大雨");
        assert_eq!(weather_code_label(75), "大雪");
        assert_eq!(weather_code_label(95), "雷暴");
        assert_eq!(weather_code_label(12345), "未知天气");
    }

    #[test]
    fn maps_wind_direction_degrees() {
        assert_eq!(wind_direction_label(0.0), "北风");
        assert_eq!(wind_direction_label(45.0), "东北风");
        assert_eq!(wind_direction_label(90.0), "东风");
        assert_eq!(wind_direction_label(180.0), "南风");
        assert_eq!(wind_direction_label(270.0), "西风");
        assert_eq!(wind_direction_label(337.0), "西北风");
    }

    #[test]
    fn maps_air_quality_labels() {
        assert_eq!(european_aqi_label(20), "良好");
        assert_eq!(european_aqi_label(80), "差");
        assert_eq!(us_aqi_label(50), "良好");
        assert_eq!(us_aqi_label(151), "不健康");
    }

    #[test]
    fn selects_capital_and_population_for_ambiguous_location() {
        let small = GeocodingResult {
            name: "Beijing".to_string(),
            latitude: 35.2,
            longitude: 110.7,
            elevation: None,
            feature_code: Some("PPL".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("山西".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: None,
        };
        let capital = GeocodingResult {
            name: "北京".to_string(),
            latitude: 39.9,
            longitude: 116.4,
            elevation: None,
            feature_code: Some("PPLC".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("北京市".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: Some(18_960_744),
        };
        let locations = vec![small, capital];
        let selected = select_location("Beijing", &locations).unwrap();
        assert_eq!(selected.name, "北京");
    }

    #[test]
    fn expands_common_translated_location_aliases() {
        assert_eq!(
            geocoding_query_names("东京"),
            vec!["Tokyo".to_string(), "东京".to_string(), "東京".to_string()]
        );
        assert_eq!(
            geocoding_query_names("日本东京"),
            vec![
                "Tokyo".to_string(),
                "日本东京".to_string(),
                "東京".to_string()
            ]
        );
        assert_eq!(
            geocoding_query_names("纽约"),
            vec!["New York".to_string(), "纽约".to_string()]
        );
        assert_eq!(
            geocoding_query_names("Beijing"),
            vec!["Beijing".to_string()]
        );
    }

    #[test]
    fn selects_japanese_tokyo_for_chinese_tokyo_alias() {
        let china_tokyo = GeocodingResult {
            name: "东京".to_string(),
            latitude: 28.0,
            longitude: 119.4,
            elevation: None,
            feature_code: Some("PPL".to_string()),
            country_code: Some("CN".to_string()),
            country: Some("中国".to_string()),
            admin1: Some("浙江".to_string()),
            admin2: None,
            timezone: Some("Asia/Shanghai".to_string()),
            population: None,
        };
        let japan_tokyo = GeocodingResult {
            name: "東京".to_string(),
            latitude: 35.6895,
            longitude: 139.69171,
            elevation: Some(44.0),
            feature_code: Some("PPLC".to_string()),
            country_code: Some("JP".to_string()),
            country: Some("日本".to_string()),
            admin1: Some("东京都".to_string()),
            admin2: None,
            timezone: Some("Asia/Tokyo".to_string()),
            population: Some(9_733_276),
        };
        let locations = vec![china_tokyo, japan_tokyo];
        let selected = select_location("东京", &locations).unwrap();
        assert_eq!(selected.country_code.as_deref(), Some("JP"));
    }
}
