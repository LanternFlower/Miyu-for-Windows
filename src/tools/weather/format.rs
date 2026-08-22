//! 把接口结果排成给模型看的文本与 JSON。
//!
//! 单位与标签集中在这里（`format_temperature` 一族、`weather_code_label`、
//! `*_aqi_label`）：同一个数值在不同接口里出现，格式必须一致，否则模型会以为
//! 是两回事。

use crate::tools::weather::*;

pub(in crate::tools::weather) fn format_open_meteo_result(
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

pub(in crate::tools::weather) fn location_json(location: &GeocodingResult) -> Value {
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

pub(in crate::tools::weather) fn daily_json(daily: &DailyWeather) -> Vec<Value> {
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

pub(in crate::tools::weather) fn format_air_quality_result(
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

pub(in crate::tools::weather) fn format_historical_result(
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

pub(in crate::tools::weather) fn historical_daily_json(daily: &HistoricalDaily) -> Vec<Value> {
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

pub(in crate::tools::weather) fn format_marine_result(
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

pub(in crate::tools::weather) fn marine_daily_json(daily: &MarineDaily) -> Vec<Value> {
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

pub(in crate::tools::weather) fn format_climate_result(
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

pub(in crate::tools::weather) fn climate_daily_json(daily: &ClimateDaily) -> Vec<Value> {
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

pub(in crate::tools::weather) fn format_elevation_result(
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

pub(in crate::tools::weather) fn value_at<T: Copy>(values: Option<&[T]>, index: usize) -> Option<T> {
    values.and_then(|values| values.get(index).copied())
}

pub(in crate::tools::weather) fn format_today(daily: &DailyWeather) -> String {
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

pub(in crate::tools::weather) fn european_aqi_label(value: u64) -> &'static str {
    match value {
        0..=20 => "良好",
        21..=40 => "尚可",
        41..=60 => "中等",
        61..=80 => "差",
        81..=100 => "很差",
        _ => "极差",
    }
}

pub(in crate::tools::weather) fn us_aqi_label(value: u64) -> &'static str {
    match value {
        0..=50 => "良好",
        51..=100 => "中等",
        101..=150 => "对敏感人群不健康",
        151..=200 => "不健康",
        201..=300 => "很不健康",
        _ => "危险",
    }
}

pub(in crate::tools::weather) fn open_meteo_source(note: &str) -> Value {
    json!({
        "name": "Open-Meteo",
        "attribution": "Weather data by Open-Meteo.com (https://open-meteo.com/)",
        "note": note,
    })
}

pub(in crate::tools::weather) fn weather_code_label(code: i64) -> &'static str {
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

pub(in crate::tools::weather) fn wind_direction_label(degrees: f64) -> &'static str {
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
