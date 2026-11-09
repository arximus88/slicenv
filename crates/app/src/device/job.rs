//! Поточне завдання принтера та його мініатюра. Мініатюра належить конкретному завданню на
//! конкретному принтері: зміна завдання чи принтера скидає її, а запізніла відповідь на
//! попередній запит відкидається за ключем.

use cloud::{DeviceId, PrinterStatus};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobKey {
    pub dev: DeviceId,
    pub subtask_id: String,
    pub task_id: String,
    pub job_id: String,
    pub name: String,
    pub file: String,
    /// Усі три ідентифікатори хмарного завдання відомі й ненульові (умова Studio для
    /// `update_slice_info`); без цього мініатюру запросити нема за чим.
    pub cloud: bool,
}

/// Тільки активні стани мають «поточне завдання».
pub fn is_active(state: Option<&str>) -> bool {
    matches!(state, Some("RUNNING" | "PAUSE" | "PREPARE" | "SLICING"))
}

fn id(value: &Option<String>) -> String {
    value
        .as_deref()
        .filter(|v| *v != "0")
        .unwrap_or_default()
        .to_owned()
}

pub fn job_key(dev: &DeviceId, status: &PrinterStatus) -> Option<JobKey> {
    if !is_active(status.gcode_state.as_deref()) {
        return None;
    }
    let key = JobKey {
        dev: dev.clone(),
        subtask_id: id(&status.subtask_id),
        task_id: id(&status.task_id),
        job_id: id(&status.job_id),
        name: status.subtask_name.clone().unwrap_or_default(),
        file: status.gcode_file.clone().unwrap_or_default(),
        cloud: !id(&status.project_id).is_empty()
            && !id(&status.profile_id).is_empty()
            && !id(&status.subtask_id).is_empty(),
    };
    // Без жодної ознаки завдання (ранній знімок) ключа ще нема.
    let known = [
        &key.subtask_id,
        &key.task_id,
        &key.job_id,
        &key.name,
        &key.file,
    ]
    .iter()
    .any(|v| !v.is_empty());
    known.then_some(key)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbState {
    /// Немає активного завдання.
    Idle,
    Loading,
    Ready,
    /// Завдання є, а мініатюри нема (локальний друк або помилка завантаження).
    Unavailable(String),
}

pub struct JobThumb<T> {
    key: Option<JobKey>,
    state: ThumbState,
    image: Option<T>,
}

impl<T> Default for JobThumb<T> {
    fn default() -> Self {
        Self {
            key: None,
            state: ThumbState::Idle,
            image: None,
        }
    }
}

/// Результат `observe`: старе зображення, яке слід звільнити, і ключ для нового запиту.
pub struct Observed<T> {
    pub release: Option<T>,
    pub fetch: Option<JobKey>,
}

impl<T> JobThumb<T> {
    #[cfg(test)]
    pub fn key(&self) -> Option<&JobKey> {
        self.key.as_ref()
    }
    pub fn state(&self) -> &ThumbState {
        &self.state
    }
    pub fn image(&self) -> Option<&T> {
        self.image.as_ref()
    }

    /// Викликати на кожен знімок вибраного принтера.
    pub fn observe(&mut self, dev: &DeviceId, status: &PrinterStatus) -> Observed<T> {
        let next = job_key(dev, status);
        if next == self.key {
            return Observed {
                release: None,
                fetch: None,
            };
        }
        let release = self.image.take();
        self.key = next.clone();
        self.state = match &next {
            None => ThumbState::Idle,
            Some(key) if key.cloud => ThumbState::Loading,
            Some(_) => ThumbState::Unavailable("local".into()),
        };
        let fetch = next.filter(|key| key.cloud);
        Observed { release, fetch }
    }

    /// Скидання при зміні принтера, відключенні чи виході з активного завдання.
    pub fn clear(&mut self) -> Option<T> {
        self.key = None;
        self.state = ThumbState::Idle;
        self.image.take()
    }

    /// Приймає відповідь лише для поточного ключа; інакше повертає зображення назад,
    /// щоб викликач його звільнив.
    pub fn accept(&mut self, key: &JobKey, result: Result<T, String>) -> Option<T> {
        if self.key.as_ref() != Some(key) {
            return result.ok();
        }
        match result {
            Ok(image) => {
                let old = self.image.replace(image);
                self.state = ThumbState::Ready;
                old
            }
            Err(reason) => {
                self.state = ThumbState::Unavailable(reason);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(name: &str) -> DeviceId {
        DeviceId::new(name)
    }
    fn running(subtask: &str, name: &str) -> PrinterStatus {
        PrinterStatus {
            gcode_state: Some("RUNNING".into()),
            subtask_id: Some(subtask.into()),
            project_id: Some("1".into()),
            profile_id: Some("2".into()),
            subtask_name: Some(name.into()),
            ..Default::default()
        }
    }

    #[test]
    fn idle_printer_has_no_job_and_early_snapshot_has_no_key() {
        let idle = PrinterStatus {
            gcode_state: Some("IDLE".into()),
            subtask_name: Some("Old".into()),
            ..Default::default()
        };
        assert!(job_key(&dev("A"), &idle).is_none());
        let early = PrinterStatus {
            gcode_state: Some("RUNNING".into()),
            subtask_id: Some("0".into()),
            ..Default::default()
        };
        assert!(job_key(&dev("A"), &early).is_none());
    }

    #[test]
    fn new_job_requests_a_fetch_once_and_drops_the_old_image() {
        let mut thumb = JobThumb::<&'static str>::default();
        let first = thumb.observe(&dev("A"), &running("10", "Part"));
        assert!(first.fetch.is_some() && first.release.is_none());
        assert_eq!(*thumb.state(), ThumbState::Loading);
        // Той самий знімок повторно: без нового запиту.
        assert!(
            thumb
                .observe(&dev("A"), &running("10", "Part"))
                .fetch
                .is_none()
        );
        let key = first.fetch.unwrap();
        assert!(thumb.accept(&key, Ok("img-10")).is_none());
        assert_eq!(thumb.image(), Some(&"img-10"));

        // Інше завдання: старе зображення віддається на звільнення, стан — Loading.
        let second = thumb.observe(&dev("A"), &running("11", "Other"));
        assert_eq!(second.release, Some("img-10"));
        assert!(second.fetch.is_some());
        assert!(thumb.image().is_none());
        assert_eq!(*thumb.state(), ThumbState::Loading);
    }

    #[test]
    fn late_answer_for_a_previous_job_or_device_is_rejected() {
        let mut thumb = JobThumb::<&'static str>::default();
        let old_key = thumb
            .observe(&dev("A"), &running("10", "Part"))
            .fetch
            .unwrap();
        let new_key = thumb
            .observe(&dev("A"), &running("11", "Other"))
            .fetch
            .unwrap();
        // Відповідь на старе завдання повертається викликачу й не стає мініатюрою.
        assert_eq!(thumb.accept(&old_key, Ok("late-10")), Some("late-10"));
        assert!(thumb.image().is_none());
        assert_eq!(*thumb.state(), ThumbState::Loading);
        assert!(thumb.accept(&new_key, Ok("img-11")).is_none());
        assert_eq!(thumb.image(), Some(&"img-11"));

        // Той самий серійний номер завдання на іншому принтері — інше завдання.
        let other_dev = thumb.observe(&dev("B"), &running("11", "Other"));
        assert_eq!(other_dev.release, Some("img-11"));
        assert!(other_dev.fetch.is_some());
    }

    #[test]
    fn finishing_or_switching_device_clears_the_thumbnail() {
        let mut thumb = JobThumb::<&'static str>::default();
        let key = thumb
            .observe(&dev("A"), &running("10", "Part"))
            .fetch
            .unwrap();
        thumb.accept(&key, Ok("img"));
        let done = PrinterStatus {
            gcode_state: Some("FINISH".into()),
            ..running("10", "Part")
        };
        let observed = thumb.observe(&dev("A"), &done);
        assert_eq!(observed.release, Some("img"));
        assert_eq!(*thumb.state(), ThumbState::Idle);
        assert!(thumb.key().is_none());

        let key = thumb
            .observe(&dev("A"), &running("12", "Next"))
            .fetch
            .unwrap();
        thumb.accept(&key, Ok("img-12"));
        assert_eq!(thumb.clear(), Some("img-12"));
        assert_eq!(*thumb.state(), ThumbState::Idle);
        // Запізніла відповідь після clear відкидається.
        assert_eq!(thumb.accept(&key, Ok("zombie")), Some("zombie"));
        assert!(thumb.image().is_none());
    }

    #[test]
    fn local_prints_and_failed_downloads_are_explicit_not_blank() {
        let mut thumb = JobThumb::<&'static str>::default();
        let local = PrinterStatus {
            gcode_state: Some("RUNNING".into()),
            subtask_id: Some("0".into()),
            project_id: Some("0".into()),
            profile_id: Some("0".into()),
            subtask_name: Some("From SD".into()),
            ..Default::default()
        };
        let observed = thumb.observe(&dev("A"), &local);
        assert!(observed.fetch.is_none(), "для локального друку запиту нема");
        assert!(matches!(thumb.state(), ThumbState::Unavailable(_)));

        let key = thumb
            .observe(&dev("A"), &running("20", "Cloud"))
            .fetch
            .unwrap();
        thumb.accept(&key, Err("мережа".into()));
        assert_eq!(*thumb.state(), ThumbState::Unavailable("мережа".into()));
        assert!(thumb.image().is_none());
    }
}
