use crate::model::{BasicAuthConfig, CompatHeaders, ServiceConnectionConfig};

pub enum EndpointLayout {
    Nrese,
    FusekiDataset,
}

pub struct ServiceTarget {
    pub label: &'static str,
    pub base_url: String,
    pub layout: EndpointLayout,
    pub basic_auth: Option<BasicAuthConfig>,
    pub default_headers: CompatHeaders,
    pub default_timeout_ms: Option<u64>,
}

impl ServiceTarget {
    pub fn nrese(config: ServiceConnectionConfig) -> Self {
        Self {
            label: "NRESE",
            base_url: config.base_url,
            layout: EndpointLayout::Nrese,
            basic_auth: config.basic_auth,
            default_headers: config.headers,
            default_timeout_ms: config.timeout_ms,
        }
    }

    pub fn fuseki(config: ServiceConnectionConfig) -> Self {
        Self {
            label: "Fuseki",
            base_url: config.base_url,
            layout: EndpointLayout::FusekiDataset,
            basic_auth: config.basic_auth,
            default_headers: config.headers,
            default_timeout_ms: config.timeout_ms,
        }
    }

    pub fn query_endpoint(&self) -> String {
        self.layout.query_endpoint(&self.base_url)
    }

    pub fn update_endpoint(&self) -> String {
        self.layout.update_endpoint(&self.base_url)
    }

    pub fn data_endpoint(&self) -> String {
        self.layout.data_endpoint(&self.base_url)
    }

    pub fn data_endpoint_base(&self) -> String {
        self.layout.data_endpoint_base(&self.base_url)
    }
}

impl EndpointLayout {
    pub fn query_endpoint(&self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/query"),
            Self::FusekiDataset => join_url(base_url, "/query"),
        }
    }

    pub fn update_endpoint(&self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/update"),
            Self::FusekiDataset => join_url(base_url, "/update"),
        }
    }

    pub fn data_endpoint(&self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/data?default"),
            Self::FusekiDataset => join_url(base_url, "/data?default"),
        }
    }

    pub fn data_endpoint_base(&self, base_url: &str) -> String {
        match self {
            Self::Nrese => join_url(base_url, "/dataset/data"),
            Self::FusekiDataset => join_url(base_url, "/data"),
        }
    }
}

fn join_url(base_url: &str, suffix: &str) -> String {
    format!("{}{}", base_url.trim_end_matches('/'), suffix)
}

#[cfg(test)]
mod tests {
    use crate::model::{CompatHeaders, ServiceConnectionConfig};

    use super::{EndpointLayout, ServiceTarget};

    #[test]
    fn nrese_layout_builds_dataset_endpoints() {
        let target = ServiceTarget::nrese(ServiceConnectionConfig {
            base_url: "http://127.0.0.1:8080/".to_owned(),
            headers: CompatHeaders::new(),
            timeout_ms: Some(25),
            basic_auth: None,
        });

        assert_eq!(
            target.query_endpoint(),
            "http://127.0.0.1:8080/dataset/query"
        );
        assert_eq!(
            target.update_endpoint(),
            "http://127.0.0.1:8080/dataset/update"
        );
        assert_eq!(
            target.data_endpoint(),
            "http://127.0.0.1:8080/dataset/data?default"
        );
        assert!(target.basic_auth.is_none());
        assert!(target.default_headers.is_empty());
        assert_eq!(target.default_timeout_ms, Some(25));
    }

    #[test]
    fn fuseki_layout_builds_dataset_endpoints() {
        assert_eq!(
            EndpointLayout::FusekiDataset.query_endpoint("http://127.0.0.1:3030/ds"),
            "http://127.0.0.1:3030/ds/query"
        );
    }
}
