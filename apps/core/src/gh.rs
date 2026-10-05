//! GitHub REST client. All HTTP goes through the injected `Runtime`,
//! so core never links a TLS stack or an async runtime.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::http::{HttpRequest, Runtime};
use crate::Error;

pub const GH_API: &str = "https://api.github.com";
pub const DEFAULT_REPO: &str = "gitlarp-history";

pub struct GhClient<'a> {
    rt: &'a dyn Runtime,
    pat: String,
    repo: String,
}

impl<'a> GhClient<'a> {
    pub fn new(rt: &'a dyn Runtime, pat: &str, repo: &str) -> Self {
        GhClient { rt, pat: pat.to_string(), repo: repo.to_string() }
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> HttpRequest {
        let mut headers = vec![
            ("Authorization".into(), format!("Bearer {}", self.pat)),
            ("Accept".into(), "application/vnd.github+json".into()),
            ("User-Agent".into(), "gitlarp".into()),
        ];
        if body.is_some() {
            headers.push(("Content-Type".into(), "application/json".into()));
        }
        HttpRequest {
            method: method.into(),
            url: format!("{GH_API}{path}"),
            headers,
            body: body.map(|b| b.to_string()),
        }
    }

    async fn attempt(&self, method: &str, path: &str, body: Option<&str>) -> Result<Value, Error> {
        let res = self
            .rt
            .fetch(self.request(method, path, body))
            .await
            .map_err(|e| Error::new(502, format!("github unreachable: {e}")))?;
        if res.status == 401 {
            return Err(Error::new(401, "invalid PAT"));
        }
        if !(200..300).contains(&res.status) {
            let msg = serde_json::from_str::<Value>(&res.body)
                .ok()
                .and_then(|b| b["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("GitHub {path} -> {}", res.status));
            return Err(Error::new(res.status, msg));
        }
        serde_json::from_str(&res.body)
            .map_err(|_| Error::new(502, format!("GitHub {path} -> bad JSON")))
    }

    /// One call, retried once (after a pause) on rate limits. A 403 is
    /// only treated as a rate limit when GitHub's message says so
    /// ("API rate limit exceeded" / "secondary rate limit"); a plain
    /// permission 403 fails immediately instead of wasting a retry.
    async fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<Value, Error> {
        match self.attempt(method, path, body).await {
            Err(e) if e.status == 429 || (e.status == 403 && is_rate_limit(&e.message)) => {
                self.rt.sleep(1000).await;
                self.attempt(method, path, body).await
            }
            r => r,
        }
    }

    pub async fn user(&self) -> Result<(u64, String), Error> {
        let v = self.call("GET", "/user", None).await?;
        let id = v["id"].as_u64().ok_or_else(|| Error::new(502, "user has no id"))?;
        let login = v["login"].as_str().ok_or_else(|| Error::new(502, "user has no login"))?;
        Ok((id, login.to_string()))
    }

    pub async fn ensure_repo(&self, login: &str) -> Result<String, Error> {
        let path = format!("/repos/{login}/{}", self.repo);
        let repo = match self.call("GET", &path, None).await {
            Ok(v) => v,
            Err(e) if e.status == 404 => {
                let body = json!({ "name": self.repo, "private": true }).to_string();
                self.call("POST", "/user/repos", Some(&body)).await?
            }
            Err(e) => return Err(e),
        };
        Ok(repo["default_branch"].as_str().unwrap_or("main").to_string())
    }

    /// `name` is a full ref like "refs/heads/main". 404 → None.
    pub async fn get_ref(&self, login: &str, name: &str) -> Result<Option<String>, Error> {
        let path = format!("/repos/{login}/{}/git/ref/{}", self.repo, short_ref(name));
        match self.call("GET", &path, None).await {
            Ok(v) => Ok(Some(
                v["object"]["sha"]
                    .as_str()
                    .ok_or_else(|| Error::new(502, "branch ref has no sha"))?
                    .to_string(),
            )),
            Err(e) if e.status == 404 => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn commit_tree(&self, login: &str, sha: &str) -> Result<String, Error> {
        let path = format!("/repos/{login}/{}/git/commits/{sha}", self.repo);
        let v = self.call("GET", &path, None).await?;
        v["tree"]["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::new(502, "commit has no tree"))
    }

    pub async fn create_blob(&self, login: &str) -> Result<String, Error> {
        let path = format!("/repos/{login}/{}/git/blobs", self.repo);
        let body = json!({ "content": "larp\n", "encoding": "utf-8" }).to_string();
        let v = self.call("POST", &path, Some(&body)).await?;
        v["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::new(502, "blob create returned no sha"))
    }

    pub async fn create_tree(&self, login: &str, blob_sha: &str) -> Result<String, Error> {
        let path = format!("/repos/{login}/{}/git/trees", self.repo);
        let body = json!({
            "tree": [{ "path": "filler.txt", "mode": "100644", "type": "blob", "sha": blob_sha }]
        })
        .to_string();
        let v = self.call("POST", &path, Some(&body)).await?;
        v["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::new(502, "tree create returned no sha"))
    }

    pub async fn create_commit(
        &self,
        login: &str,
        email: &str,
        date: &str,
        tree_sha: &str,
        parent: Option<&str>,
    ) -> Result<String, Error> {
        let path = format!("/repos/{login}/{}/git/commits", self.repo);
        let ts = format!("{date}T12:00:00Z");
        let body = json!({
            "message": date,
            "tree": tree_sha,
            "parents": parent.map(|p| vec![p]).unwrap_or_default(),
            "author": { "name": login, "email": email, "date": ts },
            "committer": { "name": login, "email": email, "date": ts },
        })
        .to_string();
        let v = self.call("POST", &path, Some(&body)).await?;
        v["sha"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| Error::new(502, "commit failed: no sha"))
    }

    pub async fn create_ref(&self, login: &str, name: &str, sha: &str) -> Result<(), Error> {
        let path = format!("/repos/{login}/{}/git/refs", self.repo);
        let body = json!({ "ref": name, "sha": sha }).to_string();
        self.call("POST", &path, Some(&body)).await.map(|_| ())
    }

    pub async fn update_ref(&self, login: &str, name: &str, sha: &str) -> Result<(), Error> {
        let path = format!("/repos/{login}/{}/git/refs/{}", self.repo, short_ref(name));
        let body = json!({ "sha": sha, "force": true }).to_string();
        self.call("PATCH", &path, Some(&body)).await.map(|_| ())
    }

    pub async fn delete_ref(&self, login: &str, name: &str) -> Result<(), Error> {
        let path = format!("/repos/{login}/{}/git/refs/{}", self.repo, short_ref(name));
        self.call("DELETE", &path, None).await.map(|_| ())
    }

    /// Contribution calendar counts, `date -> count`. The login rides
    /// in GraphQL `variables`, never string-interpolated into the query.
    pub async fn contributions_graph(&self, login: &str) -> Result<BTreeMap<String, u32>, Error> {
        let query = "query($login: String!) { user(login: $login) { contributionsCollection \
             { contributionCalendar { weeks { contributionDays { date contributionCount } } } } } }";
        let body = json!({
            "query": query,
            "variables": { "login": login }
        })
        .to_string();
        let v = self.call("POST", "/graphql", Some(&body)).await?;
        if let Some(first) = v["errors"].as_array().and_then(|a| a.first()) {
            return Err(Error::new(
                502,
                first["message"].as_str().unwrap_or("graphql error").to_string(),
            ));
        }
        let mut counts = BTreeMap::new();
        let weeks = v
            .pointer("/data/user/contributionsCollection/contributionCalendar/weeks")
            .and_then(|w| w.as_array());
        for w in weeks.into_iter().flatten() {
            for d in w["contributionDays"].as_array().into_iter().flatten() {
                if let (Some(date), Some(n)) = (d["date"].as_str(), d["contributionCount"].as_u64()) {
                    counts.insert(date.to_string(), n as u32);
                }
            }
        }
        Ok(counts)
    }
}

fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/").unwrap_or(name)
}

/// GitHub signals rate limits with "API rate limit exceeded" or
/// "You have exceeded a secondary rate limit"; other 403s are
/// permission errors that a retry cannot fix.
fn is_rate_limit(msg: &str) -> bool {
    msg.to_ascii_lowercase().contains("rate limit")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use crate::Error;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn short_ref_strips_prefix() {
        assert_eq!(short_ref("refs/heads/main"), "heads/main");
        assert_eq!(short_ref("heads/main"), "heads/main");
    }

    /// Scripted Runtime: pops one canned response per fetch.
    struct Scripted {
        calls: Mutex<Vec<HttpRequest>>,
        responses: Mutex<VecDeque<Result<HttpResponse, Error>>>,
        sleeps: Mutex<u32>,
    }

    impl Scripted {
        fn new(responses: Vec<Result<HttpResponse, Error>>) -> Scripted {
            Scripted {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
                sleeps: Mutex::new(0),
            }
        }
    }

    impl Runtime for Scripted {
        fn fetch(&self, req: HttpRequest) -> crate::http::BoxFut<Result<HttpResponse, Error>> {
            self.calls.lock().unwrap().push(req);
            let r = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(Error::new(500, "scripted: unexpected request")));
            Box::pin(std::future::ready(r))
        }
        fn sleep(&self, _ms: u64) -> crate::http::BoxFut<()> {
            *self.sleeps.lock().unwrap() += 1;
            Box::pin(std::future::ready(()))
        }
        fn random(&self, buf: &mut [u8]) {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = i as u8;
            }
        }
    }

    fn ok200(body: serde_json::Value) -> Result<HttpResponse, Error> {
        Ok(HttpResponse { status: 200, body: body.to_string() })
    }

    #[test]
    fn contributions_graph_parses_counts() {
        let calendar = serde_json::json!({
            "data": { "user": { "contributionsCollection": { "contributionCalendar": { "weeks": [
                { "contributionDays": [
                    { "date": "2026-08-20", "contributionCount": 3 },
                    { "date": "2026-08-21", "contributionCount": 0 }
                ] },
                { "contributionDays": [
                    { "date": "2026-08-22", "contributionCount": 7 }
                ] }
            ] } } } }
        });
        let m = Scripted::new(vec![ok200(calendar)]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let counts = crate::block_on(gh.contributions_graph("octocat")).unwrap();
        assert_eq!(counts.get("2026-08-20"), Some(&3));
        assert_eq!(counts.get("2026-08-21"), Some(&0), "zero days are kept");
        assert_eq!(counts.get("2026-08-22"), Some(&7));
        assert_eq!(counts.len(), 3);
        // the GraphQL query uses a $login variable, never interpolation
        let req = &m.calls.lock().unwrap()[0];
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.github.com/graphql");
        let body = req.body.as_deref().unwrap();
        assert!(body.contains("user(login: $login)"), "query uses a variable: {body}");
        let sent: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(sent["variables"]["login"], "octocat", "login sent as a GraphQL variable");
        assert!(body.contains("contributionCalendar"));
    }

    #[test]
    fn contributions_graph_empty_calendar() {
        let m = Scripted::new(vec![ok200(serde_json::json!({
            "data": { "user": { "contributionsCollection": { "contributionCalendar": { "weeks": [] } } } }
        }))]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let counts = crate::block_on(gh.contributions_graph("octocat")).unwrap();
        assert!(counts.is_empty());
    }

    #[test]
    fn contributions_graph_malformed_is_empty_not_panic() {
        // weeks key missing entirely -> graceful empty map
        let m = Scripted::new(vec![ok200(serde_json::json!({ "data": {} }))]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let counts = crate::block_on(gh.contributions_graph("octocat")).unwrap();
        assert!(counts.is_empty());
        // malformed day entries are skipped, valid ones kept
        let partial = serde_json::json!({
            "data": { "user": { "contributionsCollection": { "contributionCalendar": { "weeks": [
                { "contributionDays": [
                    { "date": "2026-08-20", "contributionCount": 2 },
                    { "date": "2026-08-21", "contributionCount": "not-a-number" },
                    { "contributionCount": 5 }
                ] }
            ] } } } }
        });
        let m = Scripted::new(vec![ok200(partial)]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let counts = crate::block_on(gh.contributions_graph("octocat")).unwrap();
        assert_eq!(counts.len(), 1);
        assert_eq!(counts.get("2026-08-20"), Some(&2));
    }

    #[test]
    fn contributions_graph_graphql_errors_surface() {
        let m = Scripted::new(vec![ok200(serde_json::json!({
            "errors": [ { "message": "Bad credentials" } ]
        }))]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let err = crate::block_on(gh.contributions_graph("octocat")).unwrap_err();
        assert_eq!(err.status, 502);
        assert_eq!(err.message, "Bad credentials");
    }

    #[test]
    fn rate_limit_403_retries_once() {
        // first call rate-limited (message says "rate limit"), retry
        // succeeds, proving call()'s 403/429 retry path works
        let m = Scripted::new(vec![
            Ok(HttpResponse { status: 403, body: "{\"message\":\"rate limited\"}".into() }),
            ok200(serde_json::json!({
                "data": { "user": { "contributionsCollection": { "contributionCalendar": { "weeks": [
                    { "contributionDays": [ { "date": "2026-08-20", "contributionCount": 1 } ] }
                ] } } } }
            })),
        ]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let counts = crate::block_on(gh.contributions_graph("octocat")).unwrap();
        assert_eq!(counts.get("2026-08-20"), Some(&1));
        assert_eq!(m.calls.lock().unwrap().len(), 2, "exactly one retry");
        assert_eq!(*m.sleeps.lock().unwrap(), 1);
    }

    #[test]
    fn rate_limit_429_retries_once() {
        let m = Scripted::new(vec![
            Ok(HttpResponse { status: 429, body: "{\"message\":\"too many\"}".into() }),
            ok200(serde_json::json!({ "id": 1, "login": "octocat" })),
        ]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let (_, login) = crate::block_on(gh.user()).unwrap();
        assert_eq!(login, "octocat");
        assert_eq!(m.calls.lock().unwrap().len(), 2);
        assert_eq!(*m.sleeps.lock().unwrap(), 1);
    }

    #[test]
    fn permission_403_fails_without_retry() {
        // a permission 403 (message has no "rate limit") must surface
        // immediately: one upstream call, no sleep
        let m = Scripted::new(vec![Ok(HttpResponse {
            status: 403,
            body: "{\"message\":\"Resource not accessible by personal access token\"}".into(),
        })]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let err = crate::block_on(gh.user()).unwrap_err();
        assert_eq!(err.status, 403);
        assert_eq!(err.message, "Resource not accessible by personal access token");
        assert_eq!(m.calls.lock().unwrap().len(), 1, "permission errors are not retried");
        assert_eq!(*m.sleeps.lock().unwrap(), 0);
    }

    #[test]
    fn is_rate_limit_matches_github_messages() {
        for hit in ["API rate limit exceeded", "You have exceeded a secondary rate limit", "rate limited"] {
            assert!(is_rate_limit(hit), "{hit}");
        }
        for miss in ["Resource not accessible by personal access token", "Not Found"] {
            assert!(!is_rate_limit(miss), "{miss}");
        }
    }

    #[test]
    fn invalid_pat_maps_401() {
        let m = Scripted::new(vec![Ok(HttpResponse {
            status: 401,
            body: "{\"message\":\"Bad credentials\"}".into(),
        })]);
        let gh = GhClient::new(&m, "pat", "gitlarp-history");
        let err = crate::block_on(gh.user()).unwrap_err();
        assert_eq!(err.status, 401);
        assert_eq!(err.message, "invalid PAT");
    }
}
