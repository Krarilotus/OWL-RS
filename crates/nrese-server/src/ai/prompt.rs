use crate::ai::types::SuggestionContext;

pub fn build_query_suggestion_prompt(
    context: &SuggestionContext,
    max_suggestions: usize,
) -> String {
    let current_query = context.current_query.as_deref().unwrap_or("(none)");

    format!(
        "Task: propose up to {max_suggestions} SPARQL queries for a semantic knowledge graph UI.\n\
Return strict JSON with the shape {{\"suggestions\":[{{\"title\":\"...\",\"explanation\":\"...\",\"sparql\":\"...\"}}]}}.\n\
No markdown. No prose outside JSON.\n\
\n\
Context:\n\
- locale: {locale}\n\
- user prompt: {prompt}\n\
- current query: {current_query}\n\
- quad count: {quad_count}\n\
- named graph count: {named_graph_count}\n\
- reasoning mode: {reasoning_mode}\n\
- reasoning profile: {reasoning_profile}\n\
\n\
Requirements:\n\
- Prefer safe read queries.\n\
- Make suggestions useful for people unfamiliar with SPARQL.\n\
- Use short human titles.\n\
- Keep explanations under 160 characters.\n\
- Generate valid SPARQL 1.1.\n\
- If the user asks for updates, include at most one write suggestion and mark it clearly in the explanation.\n\
- If schema details are uncertain, use general graph exploration patterns.\n",
        locale = context.locale,
        prompt = context.prompt,
        current_query = current_query,
        quad_count = context.quad_count,
        named_graph_count = context.named_graph_count,
        reasoning_mode = context.reasoning_mode,
        reasoning_profile = context.reasoning_profile,
    )
}
