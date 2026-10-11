//! [`Argon2Passwords`]: the password service (Plan.md P7.2, §7.2, ADR-0016).
//!
//! - **The format** is a PHC string, `$argon2id$v=19$m=…,t=…,p=…$<salt>$<hash>`:
//!   Argon2id v19 with the configured [`Argon2Params`], which are never below
//!   §7.2's baseline, a 16-byte salt from the [`SecureRandom`] port and a
//!   32-byte output.
//! - **Every new password is checked first.** [`Passwords::hash`] runs
//!   zxcvbn (score ≥ 3), with the username and the app's own words as user
//!   inputs, in the same blocking task as argon2, so nothing hashes an
//!   unchecked password. A refusal carries fleet-core's
//!   [`WeakWarning`]/[`WeakSuggestion`], mapped from zxcvbn's by exhaustive
//!   `match`es.
//! - **One bounded queue.** Every hash and every check runs in
//!   `spawn_blocking` behind a semaphore of `argon2_max_concurrent` slots.
//!   At most 4 callers per slot may wait for one (`try_acquire`); past
//!   that, the answer is `busy` at once. The slot's permit moves into the
//!   blocking task, so it's released only when argon2 is really done: a
//!   request cancelled by the timeout layer or a client disconnect can't
//!   free a slot early, and no more than `argon2_max_concurrent` hashes ever
//!   run at once, which bounds the memory. A blocking task can't be
//!   aborted; its permit and the caller's `JoinHandle` own it, and the
//!   runtime's shutdown waits for it.
//! - **The dummy hash** is made once, when the service is built, from 32
//!   random bytes with the configured parameters, through a private path
//!   without the strength check. [`Passwords::verify_unknown_user`] checks
//!   against it along exactly the path of a real check, so an unknown
//!   username costs the same time (§7.2).
//! - **Verifying** reads m, t and p from the stored string, so hashes made
//!   under older parameters still verify. It refuses anything this server
//!   doesn't write as [`VerifyError::Corrupt`], before running argon2:
//!   another algorithm or version, a string that doesn't parse, or
//!   parameters above the config's upper bounds, so a hand-written row
//!   can't make one login allocate gigabytes.

use std::sync::Arc;

use argon2::password_hash::phc::PasswordHash as Phc;
use argon2::password_hash::{Error as PhcError, PasswordHasher as _, PasswordVerifier as _};
use argon2::{Algorithm, Argon2, Params, Version};
use async_trait::async_trait;
use fleet_core::auth::{NewPassword, WeakSuggestion, WeakWarning};
use fleet_core::system::{SecureRandom, random_bytes};
use fleet_core::value::Username;
use secrecy::{ExposeSecret as _, SecretBox, SecretString};
use tokio::sync::Semaphore;
use zxcvbn::Score;
use zxcvbn::feedback::{Suggestion, Warning};

use crate::config::Argon2Params;
use crate::ports::passwords::{HashError, Passwords, VerifyError};
use crate::ports::users::PasswordHash;

/// How many callers may wait for each hashing slot.
const WAITING_PER_SLOT: usize = 4;
/// The salt's length: argon2's recommended 16 bytes.
const SALT_BYTES: usize = 16;
/// The hash's length: argon2's default 32 bytes.
const OUTPUT_BYTES: usize = 32;
/// The dummy hash's random password.
const DUMMY_PASSWORD_BYTES: usize = 32;
/// The only algorithm this server writes and accepts.
const ARGON2ID: &str = "argon2id";
/// Argon2's version 1.3, written `v=19`.
const VERSION_19: u32 = 0x13;
/// Words a strong password must not be built from, next to the username,
/// as zxcvbn recommends for site-specific terms.
const APP_WORDS: [&str; 2] = ["afkfleet", "minecraft"];

/// The password service: Argon2id and zxcvbn behind one bounded queue.
#[derive(Debug)]
pub struct Argon2Passwords {
    params: Params,
    random: Arc<dyn SecureRandom>,
    limiter: Limiter,
    dummy: PasswordHash,
}

impl Argon2Passwords {
    /// Builds the service and makes its dummy hash, one argon2 run with
    /// `params` in a blocking task.
    ///
    /// # Errors
    /// [`HashError::Random`] when the OS's randomness fails, or
    /// [`HashError::Failed`].
    pub async fn new(
        params: Argon2Params,
        random: Arc<dyn SecureRandom>,
    ) -> Result<Self, HashError> {
        let argon2 = Params::new(
            params.memory_kib(),
            params.iterations(),
            params.parallelism(),
            Some(OUTPUT_BYTES),
        )
        .map_err(failed)?;
        let limiter = Limiter::new(params.max_concurrent());
        // A password nobody knows, dropped (and zeroized) right after: the
        // dummy hash matches nothing anyone types. It skips the strength
        // check, so the public `hash` stays "every hash is checked".
        let password = SecretBox::new(Box::new(random_bytes::<DUMMY_PASSWORD_BYTES>(
            random.as_ref(),
        )?));
        let salt = random_bytes::<SALT_BYTES>(random.as_ref())?;
        let dummy = {
            let params = argon2.clone();
            limiter
                .run(move || hash_with_salt(&params, password.expose_secret(), &salt))
                .await??
        };
        Ok(Self {
            params: argon2,
            random,
            limiter,
            dummy,
        })
    }

    /// Checks `password` against `stored` in the queue.
    async fn check(&self, password: &SecretString, stored: Phc) -> Result<bool, VerifyError> {
        let password = SecretString::from(password.expose_secret());
        self.limiter
            .run(move || matches(&stored, password.expose_secret().as_bytes()))
            .await?
    }
}

#[async_trait]
impl Passwords for Argon2Passwords {
    async fn hash(
        &self,
        password: &NewPassword,
        username: &Username,
    ) -> Result<PasswordHash, HashError> {
        let salt = random_bytes::<SALT_BYTES>(self.random.as_ref())?;
        let password = SecretString::from(password.expose_secret());
        let username = username.as_str().to_owned();
        let params = self.params.clone();
        self.limiter
            .run(move || {
                let inputs = [username.as_str(), APP_WORDS[0], APP_WORDS[1]];
                check_strength(password.expose_secret(), &inputs)?;
                hash_with_salt(&params, password.expose_secret().as_bytes(), &salt)
            })
            .await?
    }

    async fn verify(
        &self,
        password: &SecretString,
        hash: &PasswordHash,
    ) -> Result<bool, VerifyError> {
        let stored = stored(hash.expose_secret())?;
        self.check(password, stored).await
    }

    async fn verify_unknown_user(&self, password: &SecretString) -> Result<bool, VerifyError> {
        let dummy = stored(self.dummy.expose_secret())?;
        // The same work as a real check; whatever it finds, an unknown user
        // never matches.
        self.check(password, dummy).await.map(|_| false)
    }
}

/// An argon2 error, or any other, as [`HashError::Failed`].
fn failed(error: impl std::error::Error + Send + Sync + 'static) -> HashError {
    HashError::Failed(Box::new(error))
}

/// Argon2id v19 of `password` with `salt`, as a PHC string.
fn hash_with_salt(
    params: &Params,
    password: &[u8],
    salt: &[u8],
) -> Result<PasswordHash, HashError> {
    let hash: Phc = Argon2::new(Algorithm::Argon2id, Version::V0x13, params.clone())
        .hash_password_with_salt(password, salt)
        .map_err(failed)?;
    PasswordHash::new(SecretString::from(hash.to_string())).map_err(failed)
}

/// Reads a stored hash, refusing anything this server doesn't write before
/// argon2 ever runs on it.
fn stored(text: &str) -> Result<Phc, VerifyError> {
    let hash = Phc::new(text).map_err(|_| VerifyError::Corrupt)?;
    if hash.algorithm.as_str() != ARGON2ID
        || hash.version != Some(VERSION_19)
        || hash.salt.is_none()
        || hash.hash.is_none()
    {
        return Err(VerifyError::Corrupt);
    }
    let params = Params::try_from(&hash).map_err(|_| VerifyError::Corrupt)?;
    let within_limits = params.m_cost() <= Argon2Params::MEMORY_KIB.max
        && params.t_cost() <= Argon2Params::ITERATIONS.max
        && params.p_cost() <= Argon2Params::PARALLELISM.max;
    if within_limits {
        Ok(hash)
    } else {
        Err(VerifyError::Corrupt)
    }
}

/// Whether `password` matches `stored`, with the stored hash's own
/// parameters. argon2 compares the outputs in constant time.
fn matches(stored: &Phc, password: &[u8]) -> Result<bool, VerifyError> {
    match Argon2::default().verify_password(password, stored) {
        Ok(()) => Ok(true),
        Err(PhcError::PasswordInvalid) => Ok(false),
        Err(error) => Err(VerifyError::Failed(Box::new(error))),
    }
}

/// zxcvbn's verdict on `password`: a score of 3 or more passes (§7.2).
fn check_strength(password: &str, inputs: &[&str]) -> Result<(), HashError> {
    let entropy = zxcvbn::zxcvbn(password, inputs);
    if entropy.score() >= Score::Three {
        return Ok(());
    }
    let feedback = entropy.feedback();
    Err(HashError::Weak {
        warning: feedback
            .and_then(zxcvbn::feedback::Feedback::warning)
            .map(weak_warning),
        suggestions: feedback.map_or_else(Vec::new, |feedback| {
            feedback
                .suggestions()
                .iter()
                .copied()
                .map(weak_suggestion)
                .collect()
        }),
    })
}

/// zxcvbn's warning as ours. No wildcard: a zxcvbn upgrade that adds a
/// warning fails to compile here instead of dropping the hint.
const fn weak_warning(warning: Warning) -> WeakWarning {
    match warning {
        Warning::StraightRowsOfKeysAreEasyToGuess => WeakWarning::StraightRowsOfKeysAreEasyToGuess,
        Warning::ShortKeyboardPatternsAreEasyToGuess => {
            WeakWarning::ShortKeyboardPatternsAreEasyToGuess
        }
        Warning::RepeatsLikeAaaAreEasyToGuess => WeakWarning::RepeatsLikeAaaAreEasyToGuess,
        Warning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess => {
            WeakWarning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess
        }
        Warning::ThisIsATop10Password => WeakWarning::ThisIsATop10Password,
        Warning::ThisIsATop100Password => WeakWarning::ThisIsATop100Password,
        Warning::ThisIsACommonPassword => WeakWarning::ThisIsACommonPassword,
        Warning::ThisIsSimilarToACommonlyUsedPassword => {
            WeakWarning::ThisIsSimilarToACommonlyUsedPassword
        }
        Warning::SequencesLikeAbcAreEasyToGuess => WeakWarning::SequencesLikeAbcAreEasyToGuess,
        Warning::RecentYearsAreEasyToGuess => WeakWarning::RecentYearsAreEasyToGuess,
        Warning::AWordByItselfIsEasyToGuess => WeakWarning::AWordByItselfIsEasyToGuess,
        Warning::DatesAreOftenEasyToGuess => WeakWarning::DatesAreOftenEasyToGuess,
        Warning::NamesAndSurnamesByThemselvesAreEasyToGuess => {
            WeakWarning::NamesAndSurnamesByThemselvesAreEasyToGuess
        }
        Warning::CommonNamesAndSurnamesAreEasyToGuess => {
            WeakWarning::CommonNamesAndSurnamesAreEasyToGuess
        }
    }
}

/// zxcvbn's suggestion as ours. No wildcard, as for the warnings.
const fn weak_suggestion(suggestion: Suggestion) -> WeakSuggestion {
    match suggestion {
        Suggestion::UseAFewWordsAvoidCommonPhrases => {
            WeakSuggestion::UseAFewWordsAvoidCommonPhrases
        }
        Suggestion::NoNeedForSymbolsDigitsOrUppercaseLetters => {
            WeakSuggestion::NoNeedForSymbolsDigitsOrUppercaseLetters
        }
        Suggestion::AddAnotherWordOrTwo => WeakSuggestion::AddAnotherWordOrTwo,
        Suggestion::CapitalizationDoesntHelpVeryMuch => {
            WeakSuggestion::CapitalizationDoesntHelpVeryMuch
        }
        Suggestion::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase => {
            WeakSuggestion::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase
        }
        Suggestion::ReversedWordsArentMuchHarderToGuess => {
            WeakSuggestion::ReversedWordsArentMuchHarderToGuess
        }
        Suggestion::PredictableSubstitutionsDontHelpVeryMuch => {
            WeakSuggestion::PredictableSubstitutionsDontHelpVeryMuch
        }
        Suggestion::UseALongerKeyboardPatternWithMoreTurns => {
            WeakSuggestion::UseALongerKeyboardPatternWithMoreTurns
        }
        Suggestion::AvoidRepeatedWordsAndCharacters => {
            WeakSuggestion::AvoidRepeatedWordsAndCharacters
        }
        Suggestion::AvoidSequences => WeakSuggestion::AvoidSequences,
        Suggestion::AvoidRecentYears => WeakSuggestion::AvoidRecentYears,
        Suggestion::AvoidYearsThatAreAssociatedWithYou => {
            WeakSuggestion::AvoidYearsThatAreAssociatedWithYou
        }
        Suggestion::AvoidDatesAndYearsThatAreAssociatedWithYou => {
            WeakSuggestion::AvoidDatesAndYearsThatAreAssociatedWithYou
        }
    }
}

/// Why the queue didn't run the work.
#[derive(Debug)]
enum Overload {
    /// Every slot and every waiting place is taken.
    Busy,
    /// The semaphore closed (it never does) or the blocking task panicked.
    Failed(Box<dyn std::error::Error + Send + Sync>),
}

impl From<Overload> for HashError {
    fn from(overload: Overload) -> Self {
        match overload {
            Overload::Busy => Self::Busy,
            Overload::Failed(source) => Self::Failed(source),
        }
    }
}

impl From<Overload> for VerifyError {
    fn from(overload: Overload) -> Self {
        match overload {
            Overload::Busy => Self::Busy,
            Overload::Failed(source) => Self::Failed(source),
        }
    }
}

/// The bounded queue in front of the blocking pool: `hashing` holds one
/// permit per slot, `waiting` the places for callers waiting for one.
#[derive(Debug, Clone)]
struct Limiter {
    waiting: Arc<Semaphore>,
    hashing: Arc<Semaphore>,
}

impl Limiter {
    fn new(slots: u32) -> Self {
        let slots = usize::try_from(slots).unwrap_or(usize::MAX);
        Self {
            waiting: Arc::new(Semaphore::new(slots.saturating_mul(WAITING_PER_SLOT))),
            hashing: Arc::new(Semaphore::new(slots)),
        }
    }

    /// Runs `work` in a blocking task once a slot is free.
    ///
    /// The caller takes a waiting place first, or gets [`Overload::Busy`] at
    /// once. Waiting for a slot is bounded by the request timeout layer, and
    /// dropping the caller there gives back only its waiting place: no work
    /// has started. The slot's permit then moves into the blocking task, so
    /// it's released only when `work` returns, even if the caller is dropped
    /// meanwhile (a timed-out request, a client that disconnected).
    async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Overload> {
        let place = self.waiting.try_acquire().map_err(|_| Overload::Busy)?;
        let permit = Arc::clone(&self.hashing)
            .acquire_owned()
            .await
            .map_err(|error| Overload::Failed(Box::new(error)))?;
        drop(place);
        // The blocking task can't be aborted: its permit and this handle
        // own it, and the runtime's shutdown waits for it (ADR-0016).
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|error| Overload::Failed(Box::new(error)))
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use core::time::Duration;
    use std::sync::mpsc;

    use argon2::password_hash::phc::PasswordHash as Phc;
    use argon2::{Algorithm, Argon2, Version};
    use fleet_core::system::RandomError;
    use fleet_testkit::system::SeededRandom;
    use rstest::rstest;
    use tokio::sync::oneshot;

    use super::*;

    /// How long a test waits for anything before it fails, so a broken
    /// queue fails the test instead of hanging it.
    const WAIT: Duration = Duration::from_secs(10);
    /// A strong password: zxcvbn's score is 4.
    const STRONG: &str = "correct horse battery staple";

    async fn within<F: Future>(future: F) -> F::Output {
        tokio::time::timeout(WAIT, future)
            .await
            .expect("the wait is bounded")
    }

    /// Waits until `condition` holds, failing after [`WAIT`].
    async fn until(condition: impl Fn() -> bool, what: &str) {
        let waited = tokio::time::timeout(WAIT, async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(waited.is_ok(), "never happened: {what}");
    }

    fn username(name: &str) -> Username {
        Username::try_from(name).unwrap()
    }

    fn new_password(text: &str) -> NewPassword {
        NewPassword::try_from(SecretString::from(text)).unwrap()
    }

    fn secret(text: &str) -> SecretString {
        SecretString::from(text)
    }

    async fn service_with(params: Argon2Params, random: &SeededRandom) -> Argon2Passwords {
        within(Argon2Passwords::new(params, Arc::new(random.clone())))
            .await
            .unwrap()
    }

    async fn service(seed: u64) -> Argon2Passwords {
        service_with(Argon2Params::default(), &SeededRandom::new(seed)).await
    }

    fn phc(hash: &PasswordHash) -> Phc {
        Phc::new(hash.expose_secret()).unwrap()
    }

    /// A PHC string made directly with argon2, for hashes this service
    /// wouldn't write.
    fn made_with(
        algorithm: Algorithm,
        version: Version,
        (m, t, p): (u32, u32, u32),
        password: &str,
    ) -> PasswordHash {
        let made: Phc = Argon2::new(algorithm, version, Params::new(m, t, p, Some(32)).unwrap())
            .hash_password_with_salt(password.as_bytes(), &[9; 16])
            .unwrap();
        PasswordHash::new(SecretString::from(made.to_string())).unwrap()
    }

    /// A blocking stand-in for argon2: it reports that it started, then
    /// waits until it's released (or the sender is dropped).
    fn stand_in() -> (
        impl FnOnce() + Send + 'static,
        oneshot::Receiver<()>,
        mpsc::Sender<()>,
    ) {
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let work = move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv();
        };
        (work, started_rx, release_tx)
    }

    // --- The format ------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hash_is_argon2id_v19_with_the_configured_parameters_a_16_byte_salt_and_a_32_byte_output()
     {
        let passwords = service(1).await;

        let hash = within(passwords.hash(&new_password(STRONG), &username("alice")))
            .await
            .unwrap();

        assert!(
            hash.expose_secret()
                .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "a PHC string with the baseline parameters"
        );
        let parsed = phc(&hash);
        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(parsed.version, Some(19));
        assert_eq!(parsed.salt.map(|salt| salt.len()), Some(16));
        assert_eq!(parsed.hash.map(|output| output.len()), Some(32));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_configured_parameters_reach_the_hash() {
        let params = Argon2Params::try_new(20_480, 3, 2, 2).unwrap();
        let passwords = service_with(params, &SeededRandom::new(1)).await;

        let hash = within(passwords.hash(&new_password(STRONG), &username("alice")))
            .await
            .unwrap();

        let parsed = Params::try_from(&phc(&hash)).unwrap();
        assert_eq!(
            (parsed.m_cost(), parsed.t_cost(), parsed.p_cost()),
            (20_480, 3, 2)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_seed_gives_the_same_hash_and_another_seed_another() {
        let first = within(
            service(7)
                .await
                .hash(&new_password(STRONG), &username("alice")),
        )
        .await
        .unwrap();
        let again = within(
            service(7)
                .await
                .hash(&new_password(STRONG), &username("alice")),
        )
        .await
        .unwrap();
        let other = within(
            service(8)
                .await
                .hash(&new_password(STRONG), &username("alice")),
        )
        .await
        .unwrap();

        assert_eq!(first.expose_secret(), again.expose_secret());
        assert_ne!(first.expose_secret(), other.expose_secret());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_random_source_fails_the_hash() {
        let random = SeededRandom::new(1);
        let passwords = service_with(Argon2Params::default(), &random).await;
        random.fail_next(RandomError::Unavailable);

        let result = within(passwords.hash(&new_password(STRONG), &username("alice"))).await;

        assert!(
            matches!(result, Err(HashError::Random(RandomError::Unavailable))),
            "{result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_random_source_fails_building_the_service() {
        let random = SeededRandom::new(1);
        random.fail_next(RandomError::Os { code: 5 });

        let result = within(Argon2Passwords::new(
            Argon2Params::default(),
            Arc::new(random.clone()),
        ))
        .await;

        assert!(
            matches!(result, Err(HashError::Random(RandomError::Os { code: 5 }))),
            "{result:?}"
        );
    }

    // --- Verifying ---------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_matches_its_own_hash_and_a_wrong_one_does_not() {
        let passwords = service(1).await;
        let hash = within(passwords.hash(&new_password(STRONG), &username("alice")))
            .await
            .unwrap();

        assert!(
            within(passwords.verify(&secret(STRONG), &hash))
                .await
                .unwrap()
        );
        assert!(
            !within(passwords.verify(&secret("correct horse battery stapl"), &hash))
                .await
                .unwrap()
        );
        assert!(!within(passwords.verify(&secret(""), &hash)).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hash_made_with_other_in_range_parameters_still_verifies() {
        let passwords = service(1).await;
        let older = made_with(Algorithm::Argon2id, Version::V0x13, (20_000, 3, 2), STRONG);

        assert!(
            within(passwords.verify(&secret(STRONG), &older))
                .await
                .unwrap()
        );
        assert!(
            !within(passwords.verify(&secret("wrong"), &older))
                .await
                .unwrap()
        );
    }

    #[rstest]
    #[case::argon2i(made_with(Algorithm::Argon2i, Version::V0x13, (19_456, 2, 1), STRONG))]
    #[case::argon2d(made_with(Algorithm::Argon2d, Version::V0x13, (19_456, 2, 1), STRONG))]
    #[case::version_16(made_with(Algorithm::Argon2id, Version::V0x10, (19_456, 2, 1), STRONG))]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hash_this_server_does_not_write_is_corrupt(#[case] hash: PasswordHash) {
        let passwords = service(1).await;

        let result = within(passwords.verify(&secret(STRONG), &hash)).await;

        assert!(matches!(result, Err(VerifyError::Corrupt)), "{result:?}");
    }

    /// A real hash with one parameter replaced. Verifying it would allocate
    /// or compute far beyond any configured limit, so it must be refused
    /// before argon2 runs.
    #[rstest]
    #[case::memory_above_the_limit("m=19456", "m=1048577")]
    #[case::iterations_above_the_limit("t=2", "t=11")]
    #[case::parallelism_above_the_limit("p=1", "p=17")]
    #[case::no_version("$v=19", "")]
    #[case::unparsable("$argon2id$", "$argon2id$!!$")]
    #[case::another_algorithm_name("$argon2id$", "$argon2id-but-not$")]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_hash_outside_the_limits_or_unreadable_is_corrupt(
        #[case] from: &str,
        #[case] to: &str,
    ) {
        let passwords = service(1).await;
        let real = made_with(Algorithm::Argon2id, Version::V0x13, (19_456, 2, 1), STRONG);
        let changed = real.expose_secret().replacen(from, to, 1);
        assert_ne!(changed, real.expose_secret(), "the case changes the hash");
        let hash = PasswordHash::new(SecretString::from(changed)).unwrap();

        let result = within(passwords.verify(&secret(STRONG), &hash)).await;

        assert!(matches!(result, Err(VerifyError::Corrupt)), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hash_without_its_output_is_corrupt() {
        let passwords = service(1).await;
        let real = made_with(Algorithm::Argon2id, Version::V0x13, (19_456, 2, 1), STRONG);
        let (without_output, _) = real.expose_secret().rsplit_once('$').unwrap();
        let hash = PasswordHash::new(SecretString::from(without_output.to_owned())).unwrap();

        let result = within(passwords.verify(&secret(STRONG), &hash)).await;

        assert!(matches!(result, Err(VerifyError::Corrupt)), "{result:?}");
    }

    // --- The dummy hash -------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn the_dummy_hash_has_the_configured_parameters() {
        let params = Argon2Params::try_new(20_480, 3, 2, 2).unwrap();
        let passwords = service_with(params, &SeededRandom::new(1)).await;

        let dummy = phc(&passwords.dummy);

        assert_eq!(dummy.algorithm.as_str(), "argon2id");
        assert_eq!(dummy.version, Some(19));
        let parsed = Params::try_from(&dummy).unwrap();
        assert_eq!(
            (parsed.m_cost(), parsed.t_cost(), parsed.p_cost()),
            (20_480, 3, 2)
        );
    }

    #[rstest]
    #[case::strong(STRONG)]
    #[case::empty("")]
    #[case::anything("hunter2")]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_user_never_matches(#[case] typed: &str) {
        let passwords = service(1).await;

        assert!(
            !within(passwords.verify_unknown_user(&secret(typed)))
                .await
                .unwrap()
        );
    }

    // --- The strength check ------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_weak_password_is_refused_with_zxcvbn_s_hints() {
        let passwords = service(1).await;

        let result =
            within(passwords.hash(&new_password("passwordpassword"), &username("alice"))).await;

        let Err(HashError::Weak {
            warning,
            suggestions,
        }) = result
        else {
            panic!("expected Weak, got {result:?}");
        };
        assert_eq!(
            warning,
            Some(WeakWarning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess)
        );
        assert_eq!(
            suggestions,
            [
                WeakSuggestion::AddAnotherWordOrTwo,
                WeakSuggestion::AvoidRepeatedWordsAndCharacters
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_built_from_the_username_is_weak_though_it_would_pass_for_another_user() {
        let passwords = service(1).await;
        let password = new_password("zephyrquokka2020");

        let theirs = within(passwords.hash(&password, &username("zephyrquokka"))).await;
        let someone_elses = within(passwords.hash(&password, &username("alice"))).await;

        assert!(matches!(theirs, Err(HashError::Weak { .. })), "{theirs:?}");
        assert!(someone_elses.is_ok(), "{someone_elses:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_password_built_from_the_app_s_name_is_weak_though_zxcvbn_alone_would_pass_it() {
        let passwords = service(1).await;
        assert!(
            zxcvbn::zxcvbn("afkfleet1987", &["alice"]).score() >= zxcvbn::Score::Three,
            "without the app's words, zxcvbn passes it"
        );

        let result =
            within(passwords.hash(&new_password("afkfleet1987"), &username("alice"))).await;

        assert!(matches!(result, Err(HashError::Weak { .. })), "{result:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn weak_errors_never_show_the_password() {
        let passwords = service(1).await;

        let error = within(passwords.hash(&new_password("passwordpassword"), &username("alice")))
            .await
            .unwrap_err();

        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("passwordpassword"), "{shown}");
    }

    #[rstest]
    #[case(
        Warning::StraightRowsOfKeysAreEasyToGuess,
        WeakWarning::StraightRowsOfKeysAreEasyToGuess
    )]
    #[case(
        Warning::ShortKeyboardPatternsAreEasyToGuess,
        WeakWarning::ShortKeyboardPatternsAreEasyToGuess
    )]
    #[case(
        Warning::RepeatsLikeAaaAreEasyToGuess,
        WeakWarning::RepeatsLikeAaaAreEasyToGuess
    )]
    #[case(
        Warning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess,
        WeakWarning::RepeatsLikeAbcAbcAreOnlySlightlyHarderToGuess
    )]
    #[case(Warning::ThisIsATop10Password, WeakWarning::ThisIsATop10Password)]
    #[case(Warning::ThisIsATop100Password, WeakWarning::ThisIsATop100Password)]
    #[case(Warning::ThisIsACommonPassword, WeakWarning::ThisIsACommonPassword)]
    #[case(
        Warning::ThisIsSimilarToACommonlyUsedPassword,
        WeakWarning::ThisIsSimilarToACommonlyUsedPassword
    )]
    #[case(
        Warning::SequencesLikeAbcAreEasyToGuess,
        WeakWarning::SequencesLikeAbcAreEasyToGuess
    )]
    #[case(
        Warning::RecentYearsAreEasyToGuess,
        WeakWarning::RecentYearsAreEasyToGuess
    )]
    #[case(
        Warning::AWordByItselfIsEasyToGuess,
        WeakWarning::AWordByItselfIsEasyToGuess
    )]
    #[case(
        Warning::DatesAreOftenEasyToGuess,
        WeakWarning::DatesAreOftenEasyToGuess
    )]
    #[case(
        Warning::NamesAndSurnamesByThemselvesAreEasyToGuess,
        WeakWarning::NamesAndSurnamesByThemselvesAreEasyToGuess
    )]
    #[case(
        Warning::CommonNamesAndSurnamesAreEasyToGuess,
        WeakWarning::CommonNamesAndSurnamesAreEasyToGuess
    )]
    fn each_zxcvbn_warning_maps_to_ours_with_the_same_text(
        #[case] theirs: Warning,
        #[case] ours: WeakWarning,
    ) {
        assert_eq!(weak_warning(theirs), ours);
        assert_eq!(ours.to_string(), theirs.to_string());
    }

    #[rstest]
    #[case(
        Suggestion::UseAFewWordsAvoidCommonPhrases,
        WeakSuggestion::UseAFewWordsAvoidCommonPhrases
    )]
    #[case(
        Suggestion::NoNeedForSymbolsDigitsOrUppercaseLetters,
        WeakSuggestion::NoNeedForSymbolsDigitsOrUppercaseLetters
    )]
    #[case(Suggestion::AddAnotherWordOrTwo, WeakSuggestion::AddAnotherWordOrTwo)]
    #[case(
        Suggestion::CapitalizationDoesntHelpVeryMuch,
        WeakSuggestion::CapitalizationDoesntHelpVeryMuch
    )]
    #[case(
        Suggestion::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase,
        WeakSuggestion::AllUppercaseIsAlmostAsEasyToGuessAsAllLowercase
    )]
    #[case(
        Suggestion::ReversedWordsArentMuchHarderToGuess,
        WeakSuggestion::ReversedWordsArentMuchHarderToGuess
    )]
    #[case(
        Suggestion::PredictableSubstitutionsDontHelpVeryMuch,
        WeakSuggestion::PredictableSubstitutionsDontHelpVeryMuch
    )]
    #[case(
        Suggestion::UseALongerKeyboardPatternWithMoreTurns,
        WeakSuggestion::UseALongerKeyboardPatternWithMoreTurns
    )]
    #[case(
        Suggestion::AvoidRepeatedWordsAndCharacters,
        WeakSuggestion::AvoidRepeatedWordsAndCharacters
    )]
    #[case(Suggestion::AvoidSequences, WeakSuggestion::AvoidSequences)]
    #[case(Suggestion::AvoidRecentYears, WeakSuggestion::AvoidRecentYears)]
    #[case(
        Suggestion::AvoidYearsThatAreAssociatedWithYou,
        WeakSuggestion::AvoidYearsThatAreAssociatedWithYou
    )]
    #[case(
        Suggestion::AvoidDatesAndYearsThatAreAssociatedWithYou,
        WeakSuggestion::AvoidDatesAndYearsThatAreAssociatedWithYou
    )]
    fn each_zxcvbn_suggestion_maps_to_ours_with_the_same_text(
        #[case] theirs: Suggestion,
        #[case] ours: WeakSuggestion,
    ) {
        assert_eq!(weak_suggestion(theirs), ours);
        assert_eq!(ours.to_string(), theirs.to_string());
    }

    // --- The queue ------------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancelled_caller_keeps_its_slot_until_the_work_finishes() {
        let limiter = Limiter::new(1);
        let (work, started, release) = stand_in();
        let caller = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.run(work).await }
        });
        within(started).await.unwrap();

        caller.abort();
        assert!(within(caller).await.unwrap_err().is_cancelled());

        assert_eq!(
            limiter.hashing.available_permits(),
            0,
            "the slot stays taken while the work runs"
        );
        let next = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.run(|| 7).await }
        });
        until(
            || limiter.waiting.available_permits() == WAITING_PER_SLOT - 1,
            "the next caller waits for the slot",
        )
        .await;
        assert!(!next.is_finished());
        release.send(()).unwrap();
        assert_eq!(within(next).await.unwrap().unwrap(), 7);
        until(
            || limiter.hashing.available_permits() == 1,
            "the slot is free again",
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_every_slot_and_waiting_place_taken_the_next_caller_is_busy_at_once() {
        let limiter = Limiter::new(1);
        let (work, started, release) = stand_in();
        let running = tokio::spawn({
            let limiter = limiter.clone();
            async move { limiter.run(work).await }
        });
        within(started).await.unwrap();
        let waiting: Vec<_> = (0..WAITING_PER_SLOT)
            .map(|index| {
                let limiter = limiter.clone();
                tokio::spawn(async move { limiter.run(move || index).await })
            })
            .collect();
        until(
            || limiter.waiting.available_permits() == 0,
            "every waiting place is taken",
        )
        .await;

        let next = tokio::time::timeout(Duration::from_secs(1), limiter.run(|| ())).await;

        assert!(
            matches!(next, Ok(Err(Overload::Busy))),
            "answered busy at once: {next:?}"
        );
        release.send(()).unwrap();
        within(running).await.unwrap().unwrap();
        for (index, waiter) in waiting.into_iter().enumerate() {
            assert_eq!(within(waiter).await.unwrap().unwrap(), index);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_task_is_a_failure_and_frees_its_slot() {
        let limiter = Limiter::new(1);

        let result = within(limiter.run(|| -> u8 { panic!("argon2 blew up") })).await;

        assert!(matches!(result, Err(Overload::Failed(_))), "{result:?}");
        assert_eq!(limiter.hashing.available_permits(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn checks_for_unknown_users_queue_and_answer_busy_like_real_ones() {
        let params = Argon2Params::try_new(19_456, 2, 1, 1).unwrap();
        let passwords = Arc::new(service_with(params, &SeededRandom::new(5)).await);
        let real = within(passwords.hash(&new_password(STRONG), &username("alice")))
            .await
            .unwrap();
        let (work, started, release) = stand_in();
        let running = tokio::spawn({
            let passwords = Arc::clone(&passwords);
            async move { passwords.limiter.run(work).await }
        });
        within(started).await.unwrap();

        let unknown = tokio::spawn({
            let passwords = Arc::clone(&passwords);
            async move { passwords.verify_unknown_user(&secret("anything")).await }
        });
        until(
            || passwords.limiter.waiting.available_permits() == WAITING_PER_SLOT - 1,
            "the unknown user's check waits for the slot",
        )
        .await;
        let fillers: Vec<_> = (1..WAITING_PER_SLOT)
            .map(|_| {
                let passwords = Arc::clone(&passwords);
                tokio::spawn(async move { passwords.limiter.run(|| ()).await })
            })
            .collect();
        until(
            || passwords.limiter.waiting.available_permits() == 0,
            "every waiting place is taken",
        )
        .await;

        let busy_unknown = tokio::time::timeout(
            Duration::from_secs(1),
            passwords.verify_unknown_user(&secret("anything")),
        )
        .await;
        let busy_real = tokio::time::timeout(
            Duration::from_secs(1),
            passwords.verify(&secret(STRONG), &real),
        )
        .await;

        assert!(
            matches!(busy_unknown, Ok(Err(VerifyError::Busy))),
            "{busy_unknown:?}"
        );
        assert!(
            matches!(busy_real, Ok(Err(VerifyError::Busy))),
            "{busy_real:?}"
        );
        release.send(()).unwrap();
        within(running).await.unwrap().unwrap();
        assert!(!within(unknown).await.unwrap().unwrap());
        for filler in fillers {
            within(filler).await.unwrap().unwrap();
        }
    }
}
