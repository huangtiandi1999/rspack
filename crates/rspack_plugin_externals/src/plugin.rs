use std::{
  fmt::Debug,
  sync::{Arc, LazyLock},
};

use regex::Regex;
use rspack_core::{
  ApplyContext, BoxModule, CompilerOptions, ContextInfo, DependencyMeta, ExternalItem,
  ExternalItemFnCtx, ExternalItemValue, ExternalModule, ExternalRequest, ExternalRequestValue,
  ExternalType, ExternalTypeEnum, ModuleDependency, ModuleExt, ModuleFactoryCreateData,
  NormalModuleFactoryFactorize, Plugin, PluginContext, ResolveOptionsWithDependencyType,
};
use rspack_error::Result;
use rspack_hook::{plugin, plugin_hook};
use rspack_plugin_javascript::dependency::{ESMImportSideEffectDependency, ImportDependency};

static UNSPECIFIED_EXTERNAL_TYPE_REGEXP: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^[a-z0-9-]+ ").expect("Invalid regex"));

#[plugin]
#[derive(Debug)]
pub struct ExternalsPlugin {
  externals: Vec<ExternalItem>,
  r#type: ExternalType,
}

impl ExternalsPlugin {
  pub fn new(r#type: ExternalType, externals: Vec<ExternalItem>) -> Self {
    Self::new_inner(externals, r#type)
  }

  fn handle_external(
    &self,
    config: &ExternalItemValue,
    r#type: Option<String>,
    dependency: &dyn ModuleDependency,
  ) -> Option<ExternalModule> {
    let (external_module_config, external_module_type) = match config {
      ExternalItemValue::String(config) => {
        let (external_type, config) =
          if let Some((external_type, new_config)) = parse_external_type_from_str(config) {
            (external_type, new_config)
          } else {
            (self.r#type.clone(), config.to_owned())
          };
        (
          ExternalRequest::Single(ExternalRequestValue::new(config, None)),
          external_type,
        )
      }
      ExternalItemValue::Array(arr) => {
        let mut iter = arr.iter().peekable();
        let primary = iter.next()?;
        let (external_type, primary) =
          if let Some((external_type, new_primary)) = parse_external_type_from_str(primary) {
            (external_type, new_primary)
          } else {
            (self.r#type.clone(), primary.to_owned())
          };
        let rest = iter.peek().is_some().then(|| iter.cloned().collect());
        (
          ExternalRequest::Single(ExternalRequestValue::new(primary, rest)),
          external_type,
        )
      }
      ExternalItemValue::Bool(config) => {
        if *config {
          (
            ExternalRequest::Single(ExternalRequestValue::new(
              dependency.request().to_string(),
              None,
            )),
            self.r#type.clone(),
          )
        } else {
          return None;
        }
      }
      ExternalItemValue::Object(map) => (
        ExternalRequest::Map(
          map
            .iter()
            .map(|(k, v)| {
              let mut iter = v.iter().peekable();
              let primary = iter.next().expect("should have at least one value");
              let rest = iter.peek().is_some().then(|| iter.cloned().collect());
              (
                k.clone(),
                ExternalRequestValue::new(primary.to_owned(), rest),
              )
            })
            .collect(),
        ),
        self.r#type.clone(),
      ),
    };

    fn parse_external_type_from_str(v: &str) -> Option<(ExternalType, String)> {
      if UNSPECIFIED_EXTERNAL_TYPE_REGEXP.is_match(v)
        && let Some((t, c)) = v.split_once(' ')
      {
        return Some((t.to_owned(), c.to_owned()));
      }
      None
    }

    let dependency_meta: DependencyMeta = DependencyMeta {
      attributes: dependency.get_attributes().cloned(),
      external_type: {
        if dependency
          .as_any()
          .downcast_ref::<ImportDependency>()
          .is_some()
        {
          Some(ExternalTypeEnum::Import)
        } else if dependency
          .as_any()
          .downcast_ref::<ESMImportSideEffectDependency>()
          .is_some()
        {
          Some(ExternalTypeEnum::Module)
        } else {
          None
        }
      },
    };

    Some(ExternalModule::new(
      external_module_config,
      r#type.unwrap_or(external_module_type),
      dependency.request().to_owned(),
      dependency_meta,
    ))
  }
}

#[plugin_hook(NormalModuleFactoryFactorize for ExternalsPlugin)]
async fn factorize(&self, data: &mut ModuleFactoryCreateData) -> Result<Option<BoxModule>> {
  let dependency = data.dependencies[0]
    .as_module_dependency()
    .expect("should be module dependency");
  let context = &data.context;
  for external_item in &self.externals {
    match external_item {
      ExternalItem::Object(eh) => {
        let request = dependency.request();

        if let Some(value) = eh.get(request) {
          let maybe_module = self.handle_external(value, None, dependency);
          return Ok(maybe_module.map(|i| i.boxed()));
        }
      }
      ExternalItem::RegExp(r) => {
        let request = dependency.request();
        if r.test(request) {
          let maybe_module = self.handle_external(
            &ExternalItemValue::String(request.to_string()),
            None,
            dependency,
          );
          return Ok(maybe_module.map(|i| i.boxed()));
        }
      }
      ExternalItem::String(s) => {
        let request = dependency.request();
        if s == request {
          let maybe_module = self.handle_external(
            &ExternalItemValue::String(request.to_string()),
            None,
            dependency,
          );
          return Ok(maybe_module.map(|i| i.boxed()));
        }
      }
      ExternalItem::Fn(f) => {
        let request = dependency.request();
        let result = f(ExternalItemFnCtx {
          context: context.to_string(),
          request: request.to_string(),
          dependency_type: dependency.category().to_string(),
          context_info: ContextInfo {
            issuer: data
              .issuer
              .clone()
              .map(|i| i.to_string())
              .unwrap_or_default(),
            issuer_layer: data.issuer_layer.clone(),
          },
          resolve_options_with_dependency_type: ResolveOptionsWithDependencyType {
            resolve_options: data
              .resolve_options
              .clone()
              .map(|r| Box::new(Arc::unwrap_or_clone(r))),
            resolve_to_context: false,
            dependency_category: *data
              .dependencies
              .first()
              .expect("Expected at least one dependency")
              .category(),
          },
          resolver_factory: data.resolver_factory.clone(),
        })
        .await?;
        if let Some(r) = result.result {
          let maybe_module = self.handle_external(&r, result.external_type, dependency);
          return Ok(maybe_module.map(|i| i.boxed()));
        }
      }
    }
  }
  Ok(None)
}

impl Plugin for ExternalsPlugin {
  fn name(&self) -> &'static str {
    "rspack.ExternalsPlugin"
  }

  fn apply(&self, ctx: PluginContext<&mut ApplyContext>, _options: &CompilerOptions) -> Result<()> {
    ctx
      .context
      .normal_module_factory_hooks
      .factorize
      .tap(factorize::new(self));
    Ok(())
  }
}
