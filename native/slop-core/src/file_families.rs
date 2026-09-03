use pyo3::prelude::*;
use pyo3::types::PyList;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

const COMPONENTS: [(&str, f64); 6] = [
    ("structure", 0.35),
    ("api", 0.20),
    ("fields", 0.15),
    ("calls", 0.15),
    ("control", 0.10),
    ("imports", 0.05),
];

#[derive(Clone, Debug)]
struct Profile {
    sets: [HashSet<String>; 6],
    lsh_features: HashSet<String>,
}

#[derive(Clone, Debug)]
struct Similarity {
    score: f64,
    containment: f64,
    shared_features: usize,
    components: Vec<(String, f64)>,
}

type PythonSimilarity = (f64, f64, usize, Vec<(String, f64)>);

fn set_attr(value: &Bound<'_, PyAny>, name: &str) -> PyResult<HashSet<String>> {
    value.getattr(name)?.extract()
}

fn profile_from(value: &Bound<'_, PyAny>) -> PyResult<Profile> {
    let structure = set_attr(value, "structure")?;
    let api = set_attr(value, "api")?;
    let fields = set_attr(value, "fields")?;
    let calls = set_attr(value, "calls")?;
    let control = set_attr(value, "control")?;
    let imports = set_attr(value, "imports")?;
    let mut lsh_features = structure.clone();
    lsh_features.extend(api.iter().cloned());
    lsh_features.extend(fields.iter().cloned());
    lsh_features.extend(calls.iter().cloned());
    lsh_features.extend(control.iter().cloned());
    Ok(Profile {
        sets: [structure, api, fields, calls, control, imports],
        lsh_features,
    })
}

fn profiles_from(values: &Bound<'_, PyList>) -> PyResult<Vec<Profile>> {
    values.iter().map(|value| profile_from(&value)).collect()
}

fn decimal_round(value: f64, digits: i32) -> f64 {
    let scale = 10_f64.powi(digits);
    (value * scale).round_ties_even() / scale
}

fn jaccard(left: &HashSet<String>, right: &HashSet<String>) -> Option<f64> {
    let union = left.len() + right.len() - left.intersection(right).count();
    (union != 0).then(|| left.intersection(right).count() as f64 / union as f64)
}

fn jaccard_upper_bound(left: &HashSet<String>, right: &HashSet<String>) -> Option<f64> {
    if left.is_empty() && right.is_empty() {
        return None;
    }
    let larger = left.len().max(right.len());
    Some(left.len().min(right.len()) as f64 / larger as f64)
}

fn compare(left: &Profile, right: &Profile) -> Similarity {
    let mut components = Vec::with_capacity(COMPONENTS.len());
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for (index, (name, weight)) in COMPONENTS.iter().enumerate() {
        let Some(value) = jaccard(&left.sets[index], &right.sets[index]) else {
            continue;
        };
        components.push(((*name).to_owned(), decimal_round(value, 4)));
        weighted_sum += weight * value;
        weight_sum += weight;
    }
    let intersection = left.lsh_features.intersection(&right.lsh_features).count();
    let denominator = left.lsh_features.len().min(right.lsh_features.len()).max(1);
    Similarity {
        score: decimal_round(
            if weight_sum == 0.0 {
                0.0
            } else {
                weighted_sum / weight_sum
            },
            4,
        ),
        containment: decimal_round(intersection as f64 / denominator as f64, 4),
        shared_features: intersection,
        components,
    }
}

fn safe_bound(left: &Profile, right: &Profile) -> Option<f64> {
    let mut weighted = 0.0;
    let mut weight_sum = 0.0;
    for (index, (_, weight)) in COMPONENTS.iter().enumerate() {
        let Some(upper) = jaccard_upper_bound(&left.sets[index], &right.sets[index]) else {
            continue;
        };
        weighted += weight * upper;
        weight_sum += weight;
    }
    (weight_sum != 0.0).then_some(weighted / weight_sum)
}

fn exact_pairs(
    profiles: &[Profile],
    min_similarity: f64,
    min_shared_features: usize,
) -> (Vec<(usize, usize)>, usize) {
    let pair_universe = profiles
        .len()
        .saturating_mul(profiles.len().saturating_sub(1))
        / 2;
    let mut pairs = Vec::new();
    for left in 0..profiles.len() {
        for right in (left + 1)..profiles.len() {
            if profiles[left]
                .lsh_features
                .len()
                .min(profiles[right].lsh_features.len())
                < min_shared_features
            {
                continue;
            }
            if safe_bound(&profiles[left], &profiles[right])
                .is_some_and(|bound| bound >= min_similarity)
            {
                pairs.push((left, right));
            }
        }
    }
    (pairs, pair_universe)
}

fn cached_similarity<'a>(
    profiles: &[Profile],
    cache: &'a mut HashMap<(usize, usize), Similarity>,
    left: usize,
    right: usize,
) -> &'a Similarity {
    let key = if left <= right {
        (left, right)
    } else {
        (right, left)
    };
    cache
        .entry(key)
        .or_insert_with(|| compare(&profiles[key.0], &profiles[key.1]))
}

type GroupResult = (Vec<usize>, Vec<(f64, f64, usize)>);

fn build_groups(
    profiles: &[Profile],
    pairs: Vec<(usize, usize)>,
    min_similarity: f64,
    min_shared_features: usize,
    max_family_size: usize,
) -> (Vec<GroupResult>, usize) {
    let mut cache = HashMap::new();
    let mut edges = Vec::new();
    for (left, right) in pairs {
        let similarity = cached_similarity(profiles, &mut cache, left, right);
        if similarity.score >= min_similarity && similarity.shared_features >= min_shared_features {
            edges.push((left, right, similarity.score, similarity.containment));
        }
    }
    edges.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.3.partial_cmp(&left.3).unwrap_or(Ordering::Equal))
    });

    let mut groups: Vec<Option<HashSet<usize>>> = (0..profiles.len())
        .map(|index| Some(HashSet::from([index])))
        .collect();
    let mut owner: Vec<usize> = (0..profiles.len()).collect();
    for (left, right, _, _) in edges {
        let left_owner = owner[left];
        let right_owner = owner[right];
        if left_owner == right_owner {
            continue;
        }
        let left_group = groups[left_owner].as_ref().expect("live left group");
        let right_group = groups[right_owner].as_ref().expect("live right group");
        if left_group.len() + right_group.len() > max_family_size {
            continue;
        }
        let cross_pairs: Vec<(usize, usize)> = left_group
            .iter()
            .flat_map(|&a| right_group.iter().map(move |&b| (a, b)))
            .collect();
        let mut complete_link = true;
        for (a, b) in cross_pairs {
            let similarity = cached_similarity(profiles, &mut cache, a, b);
            if similarity.score < min_similarity || similarity.shared_features < min_shared_features
            {
                complete_link = false;
                break;
            }
        }
        if !complete_link {
            continue;
        }
        let mut merged = groups[left_owner].take().expect("live left group");
        merged.extend(groups[right_owner].take().expect("live right group"));
        for &member in &merged {
            owner[member] = left_owner;
        }
        groups[left_owner] = Some(merged);
    }

    let mut results = Vec::new();
    for group in groups.into_iter().flatten() {
        if group.len() < 2 {
            continue;
        }
        let mut members: Vec<usize> = group.into_iter().collect();
        members.sort_unstable();
        let mut similarities = Vec::new();
        for left_pos in 0..members.len() {
            for right_pos in (left_pos + 1)..members.len() {
                let similarity =
                    cached_similarity(profiles, &mut cache, members[left_pos], members[right_pos]);
                similarities.push((
                    similarity.score,
                    similarity.containment,
                    similarity.shared_features,
                ));
            }
        }
        results.push((members, similarities));
    }
    (results, cache.len())
}

#[pyfunction]
pub(crate) fn audit_file_family_compare(
    left: &Bound<'_, PyAny>,
    right: &Bound<'_, PyAny>,
) -> PyResult<PythonSimilarity> {
    let similarity = compare(&profile_from(left)?, &profile_from(right)?);
    Ok((
        similarity.score,
        similarity.containment,
        similarity.shared_features,
        similarity.components,
    ))
}

#[pyfunction]
pub(crate) fn audit_file_family_exact_pairs(
    py: Python<'_>,
    values: &Bound<'_, PyList>,
    min_similarity: f64,
    min_shared_features: usize,
) -> PyResult<(Vec<(usize, usize)>, usize)> {
    let profiles = profiles_from(values)?;
    Ok(py.detach(move || exact_pairs(&profiles, min_similarity, min_shared_features)))
}

#[pyfunction]
pub(crate) fn audit_file_family_groups(
    py: Python<'_>,
    values: &Bound<'_, PyList>,
    pairs: Vec<(usize, usize)>,
    min_similarity: f64,
    min_shared_features: usize,
    max_family_size: usize,
) -> PyResult<(Vec<GroupResult>, usize)> {
    let profiles = profiles_from(values)?;
    Ok(py.detach(move || {
        build_groups(
            &profiles,
            pairs,
            min_similarity,
            min_shared_features,
            max_family_size,
        )
    }))
}
