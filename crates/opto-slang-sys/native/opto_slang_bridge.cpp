// SPDX-FileCopyrightText: 2026 Zhengyi Zhang
// SPDX-License-Identifier: GPL-3.0-only

#include "opto_slang_internal.h"

#include "slang/ast/Compilation.h"
#include "slang/ast/ASTVisitor.h"
#include "slang/ast/EvalContext.h"
#include "slang/ast/Expression.h"
#include "slang/ast/ASTContext.h"
#include "slang/ast/SemanticFacts.h"
#include "slang/ast/TimingControl.h"
#include "slang/ast/expressions/AssignmentExpressions.h"
#include "slang/ast/expressions/CallExpression.h"
#include "slang/ast/expressions/ConversionExpression.h"
#include "slang/ast/expressions/LiteralExpressions.h"
#include "slang/ast/expressions/MiscExpressions.h"
#include "slang/ast/expressions/Operator.h"
#include "slang/ast/expressions/OperatorExpressions.h"
#include "slang/ast/expressions/SelectExpressions.h"
#include "slang/ast/symbols/BlockSymbols.h"
#include "slang/ast/symbols/CompilationUnitSymbols.h"
#include "slang/ast/symbols/InstanceSymbols.h"
#include "slang/ast/symbols/MemberSymbols.h"
#include "slang/ast/symbols/ParameterSymbols.h"
#include "slang/ast/symbols/PortSymbols.h"
#include "slang/ast/symbols/VariableSymbols.h"
#include "slang/ast/types/AllTypes.h"
#include "slang/ast/statements/ConditionalStatements.h"
#include "slang/ast/statements/LoopStatements.h"
#include "slang/ast/statements/MiscStatements.h"
#include "slang/ast/types/Type.h"
#include "slang/diagnostics/DiagnosticEngine.h"
#include "slang/driver/CompatSettings.h"
#include "slang/driver/Driver.h"
#include "slang/numeric/SVInt.h"
#include "slang/parsing/Parser.h"
#include "slang/parsing/Preprocessor.h"
#include "slang/syntax/AllSyntax.h"
#include "slang/syntax/SyntaxTree.h"
#include "slang/syntax/SyntaxVisitor.h"
#include "slang/text/SourceManager.h"
#include "slang/util/Bag.h"
#include "slang/util/ThreadPool.h"

#include <algorithm>
#include <cstdint>
#include <filesystem>
#include <exception>
#include <iterator>
#include <limits>
#include <map>
#include <memory>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

using namespace slang;
using namespace slang::ast;
using namespace slang::driver;
using namespace slang::syntax;

namespace {

OptoSlangStatus fail(OptoSlangCompiler* compiler, std::string_view message) noexcept {
    if (compiler) {
        try {
            compiler->last_error.assign(message);
        } catch (...) {
            compiler->last_error.clear();
        }
    }
    return OPTO_SLANG_ERROR;
}

OptoSlangStatus require_compiler(OptoSlangCompiler* compiler) noexcept {
    if (!compiler) {
        return OPTO_SLANG_ERROR;
    }
    return OPTO_SLANG_OK;
}

std::string copy_string(std::string_view text) {
    if (text.empty()) {
        return {};
    }
    return std::string(text.data(), text.size());
}

struct Verilog2005SyntaxValidator : SyntaxVisitor<Verilog2005SyntaxValidator> {
    std::optional<std::string> error;

    void handle(const IntegerTypeSyntax& type) {
        if (!error && type.keyword.kind == parsing::TokenKind::IntegerKeyword &&
            !type.dimensions.empty()) {
            error = "the Verilog integer type cannot have packed dimensions";
        }
        visitDefault(type);
    }

    void handle(const VariableDimensionSyntax& dimension) {
        if (!error && dimension.specifier &&
            dimension.specifier->kind == SyntaxKind::RangeDimensionSpecifier) {
            const auto& range = dimension.specifier->as<RangeDimensionSpecifierSyntax>();
            if (range.selector->kind == SyntaxKind::BitSelect) {
                error = "Verilog dimensions require an explicit msb:lsb range";
            }
        }
        visitDefault(dimension);
    }

    void handle(const PortDeclarationSyntax& port) {
        if (!error && std::ranges::any_of(port.declarators, [](const auto* declarator) {
                return declarator && !declarator->dimensions.empty();
            })) {
            error = "unpacked array ports require SystemVerilog";
        }
        visitDefault(port);
    }

    void handle(const ImplicitAnsiPortSyntax& port) {
        if (!error && port.declarator->initializer) {
            error = "ANSI port initializers require SystemVerilog";
        }
        visitDefault(port);
    }

    void handle(const ParameterPortListSyntax& parameters) {
        if (!error && std::ranges::any_of(parameters.declarations, [](const auto* declaration) {
                return declaration && !declaration->keyword;
            })) {
            error = "parameter port declarations require the 'parameter' keyword in Verilog-2005";
        }
        visitDefault(parameters);
    }
};

void validate_language_syntax(const SyntaxTree& tree, LanguageVersion language) {
    if (language != LanguageVersion::v1364_2005) {
        return;
    }
    Verilog2005SyntaxValidator validator;
    tree.root().visit(validator);
    if (validator.error) {
        throw std::runtime_error(*validator.error);
    }
}

OptoSlangSourceUnit& active_unit(OptoSlangCompiler& compiler) {
    if (compiler.units.empty()) {
        throw std::runtime_error("no active slang source unit");
    }
    return compiler.units.back();
}

std::vector<std::string> format_defines(const OptoSlangSourceUnit& unit) {
    std::vector<std::string> defines;
    defines.reserve(unit.defines.size());
    for (const auto& [name, value] : unit.defines) {
        if (value) {
            defines.push_back(name + "=" + *value);
        } else {
            defines.push_back(name);
        }
    }
    return defines;
}

std::string diagnostics_to_string(Driver& driver, const Diagnostics& diagnostics) {
    if (diagnostics.empty()) {
        return "slang compilation failed";
    }
    return DiagnosticEngine::reportAll(driver.sourceManager, diagnostics);
}

bool has_source_files(const OptoSlangCompiler& compiler) {
    return std::ranges::any_of(
        compiler.units,
        [](const OptoSlangSourceUnit& unit) { return !unit.files.empty(); });
}

std::unique_ptr<Compilation> create_compilation(
    const OptoSlangCompiler& compiler,
    Driver& driver) {
    driver.addStandardArgs();
    switch (compiler.language) {
        case slang::LanguageVersion::v1364_2005:
            driver.options.languageVersion = "1364-2005";
            break;
        case slang::LanguageVersion::v1800_2017:
            driver.options.languageVersion = "1800-2017";
            break;
        default:
            throw std::runtime_error("unsupported slang language version");
    }
    if (compiler.top) {
        driver.options.topModules.push_back(*compiler.top);
    }
    driver.options.numThreads = compiler.max_threads;
    driver.options.compilationFlags.at(CompilationFlags::IgnoreUnknownModules) = true;
    if (compiler.max_threads != 1) {
        driver.threadPool = std::make_shared<ThreadPool>(compiler.max_threads);
    }

    std::unordered_set<std::string> primary_paths;
    for (const auto& unit : compiler.units) {
        for (const auto& file : unit.files) {
            primary_paths.insert(file.path);
        }
    }
    std::unordered_map<std::string, std::string> dependency_text;
    for (const auto& unit : compiler.units) {
        for (const auto& dependency : unit.dependencies) {
            if (primary_paths.contains(dependency.path)) {
                continue;
            }
            if (!dependency.text) {
                throw std::runtime_error(
                    "source dependency has no captured text for '" +
                    dependency.path + "'");
            }
            auto [found, inserted] =
                dependency_text.emplace(dependency.path, *dependency.text);
            if (!inserted && found->second != *dependency.text) {
                throw std::runtime_error(
                    "source dependency has conflicting snapshots for '" +
                    dependency.path + "'");
            }
        }
    }
    for (const auto& [path, text] : dependency_text) {
        auto buffer = driver.sourceManager.assignText(path, text);
        driver.sourceManager.setBufferKind(
            buffer.id,
            SourceManager::BufferKind::IncludeFile);
    }

    struct ParseJob {
        std::vector<SourceBuffer> buffers;
        Bag options;
    };
    std::vector<ParseJob> parse_jobs;
    parse_jobs.reserve(compiler.units.size());
    for (const auto& unit : compiler.units) {
        if (unit.files.empty()) {
            throw std::runtime_error("slang source unit has no input files");
        }
        std::vector<SourceBuffer> buffers;
        buffers.reserve(unit.files.size());
        for (const auto& file : unit.files) {
            auto buffer = [&]() -> SourceBuffer {
                if (file.text) {
                    return driver.sourceManager.assignText(file.path, *file.text);
                }
                auto loaded = driver.sourceManager.readSource(file.path);
                if (!loaded) {
                    throw std::runtime_error(
                        "failed to read '" + file.path + "': " +
                        loaded.error().message());
                }
                return *loaded;
            }();
            driver.sourceManager.setBufferKind(
                buffer.id,
                SourceManager::BufferKind::DesignFile);
            buffers.push_back(buffer);
        }
        auto parse_options = driver.createParseOptionBag();
        auto preprocessor = parse_options.getOrDefault<parsing::PreprocessorOptions>();
        preprocessor.languageVersion = compiler.language;
        preprocessor.predefines = format_defines(unit);
        preprocessor.additionalIncludePaths.clear();
        for (const auto& path : unit.include_dirs) {
            preprocessor.additionalIncludePaths.emplace_back(path);
        }
        parse_options.set(std::move(preprocessor));
        auto lexer = parse_options.getOrDefault<parsing::LexerOptions>();
        lexer.languageVersion = compiler.language;
        for (std::string_view prefix : {"pragma", "synthesis"}) {
            lexer.commentHandlers[prefix]["translate_off"] = {
                parsing::CommentHandler::TranslateOff,
                "translate_on",
            };
        }
        parse_options.set(std::move(lexer));
        auto parser = parse_options.getOrDefault<parsing::ParserOptions>();
        parser.languageVersion = compiler.language;
        parse_options.set(std::move(parser));
        parse_jobs.push_back(ParseJob{
            std::move(buffers),
            std::move(parse_options),
        });
    }

    std::vector<std::shared_ptr<SyntaxTree>> syntax_trees(parse_jobs.size());
    std::vector<std::exception_ptr> parse_errors(parse_jobs.size());
    auto parse = [&](size_t index) {
        try {
            auto& job = parse_jobs[index];
            syntax_trees[index] = SyntaxTree::fromBuffers(
                job.buffers,
                driver.sourceManager,
                job.options);
            if (!syntax_trees[index]) {
                throw std::runtime_error(
                    "slang failed to parse in-memory source unit");
            }
            validate_language_syntax(*syntax_trees[index], compiler.language);
        } catch (...) {
            parse_errors[index] = std::current_exception();
        }
    };
    if (driver.threadPool && parse_jobs.size() > 1) {
        driver.threadPool->detach_loop(size_t(0), parse_jobs.size(), parse);
        driver.threadPool->wait();
    } else {
        for (size_t index = 0; index < parse_jobs.size(); ++index) {
            parse(index);
        }
    }
    for (auto& error : parse_errors) {
        if (error) {
            std::rethrow_exception(error);
        }
    }

    auto compilation_options = driver.createOptionBag();
    auto ast_options = compilation_options.getOrDefault<CompilationOptions>();
    ast_options.languageVersion = compiler.language;
    compilation_options.set(std::move(ast_options));
    auto compilation = std::make_unique<Compilation>(std::move(compilation_options));
    for (auto& tree : syntax_trees) {
        compilation->addSyntaxTree(std::move(tree));
    }
    const auto& parse_diagnostics = compilation->getParseDiagnostics();
    if (std::ranges::any_of(
            parse_diagnostics,
            [](const Diagnostic& diagnostic) { return diagnostic.isError(); })) {
        throw std::runtime_error(diagnostics_to_string(driver, parse_diagnostics));
    }
    return compilation;
}

void bind_procedural_bodies(
    const Scope& scope,
    std::unordered_set<const InstanceBodySymbol*>& visited_bodies) {
    for (const auto& symbol : scope.members()) {
        switch (symbol.kind) {
            case SymbolKind::ProceduralBlock:
                static_cast<void>(symbol.as<ProceduralBlockSymbol>().getBody());
                break;
            case SymbolKind::StatementBlock:
                bind_procedural_bodies(
                    symbol.as<StatementBlockSymbol>(), visited_bodies);
                break;
            case SymbolKind::GenerateBlock: {
                const auto& block = symbol.as<GenerateBlockSymbol>();
                if (!block.isUninstantiated) {
                    bind_procedural_bodies(block, visited_bodies);
                }
                break;
            }
            case SymbolKind::GenerateBlockArray:
                for (auto* block : symbol.as<GenerateBlockArraySymbol>().entries) {
                    if (block && !block->isUninstantiated) {
                        bind_procedural_bodies(*block, visited_bodies);
                    }
                }
                break;
            case SymbolKind::Instance: {
                const auto& body = symbol.as<InstanceSymbol>().body;
                if (visited_bodies.insert(&body).second) {
                    bind_procedural_bodies(body, visited_bodies);
                }
                break;
            }
            case SymbolKind::InstanceArray:
                for (auto* element : symbol.as<InstanceArraySymbol>().elements) {
                    if (!element) {
                        continue;
                    }
                    if (element->kind == SymbolKind::InstanceArray) {
                        bind_procedural_bodies(
                            element->as<InstanceArraySymbol>(), visited_bodies);
                    } else if (element->kind == SymbolKind::Instance) {
                        const auto& body = element->as<InstanceSymbol>().body;
                        if (visited_bodies.insert(&body).second) {
                            bind_procedural_bodies(body, visited_bodies);
                        }
                    }
                }
                break;
            default:
                break;
        }
    }
}

} // namespace

extern "C" {

OptoSlangCompiler* opto_slang_compiler_new(void) {
    try {
        return new OptoSlangCompiler();
    } catch (...) {
        return nullptr;
    }
}

void opto_slang_compiler_free(OptoSlangCompiler* compiler) {
    delete compiler;
}

OptoSlangStatus opto_slang_compiler_begin_source_unit(OptoSlangCompiler* compiler) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!compiler->units.empty() && compiler->units.back().files.empty()) {
        return fail(compiler, "previous slang source unit has no input files");
    }
    try {
        compiler->units.emplace_back();
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while starting a slang source unit");
    }
}

OptoSlangStatus opto_slang_compiler_add_source_file(
    OptoSlangCompiler* compiler,
    const char* path,
    const char* text) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!path || !text) {
        return fail(compiler, "input source path or text is null");
    }
    try {
        active_unit(*compiler).files.push_back({path, text});
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while adding a slang source file");
    }
}

OptoSlangStatus opto_slang_compiler_add_source_path(
    OptoSlangCompiler* compiler,
    const char* path) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!path) {
        return fail(compiler, "input source path is null");
    }
    try {
        active_unit(*compiler).files.push_back({path, std::nullopt});
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while adding a slang source path");
    }
}

OptoSlangStatus opto_slang_compiler_add_source_dependency(
    OptoSlangCompiler* compiler,
    const char* path,
    const char* text) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!path || !text) {
        return fail(compiler, "source dependency path or text is null");
    }
    try {
        active_unit(*compiler).dependencies.push_back({path, text});
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while adding a slang source dependency");
    }
}

OptoSlangStatus opto_slang_compiler_add_include_dir(
    OptoSlangCompiler* compiler,
    const char* path) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!path) {
        return fail(compiler, "include directory path is null");
    }
    try {
        active_unit(*compiler).include_dirs.emplace_back(path);
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while adding a slang include directory");
    }
}

OptoSlangStatus opto_slang_compiler_add_define(
    OptoSlangCompiler* compiler,
    const char* name,
    const char* value) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!name) {
        return fail(compiler, "define name is null");
    }
    try {
        if (value) {
            active_unit(*compiler).defines.emplace_back(name, std::string(value));
        } else {
            active_unit(*compiler).defines.emplace_back(name, std::nullopt);
        }
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while adding a slang define");
    }
}

OptoSlangStatus opto_slang_compiler_set_top(
    OptoSlangCompiler* compiler,
    const char* top) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!top) {
        return fail(compiler, "top module name is null");
    }
    try {
        compiler->top = top;
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown failure while setting the slang top module");
    }
}

OptoSlangStatus opto_slang_compiler_set_language(
    OptoSlangCompiler* compiler,
    int language) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    switch (language) {
        case 0:
            compiler->language = slang::LanguageVersion::v1364_2005;
            return OPTO_SLANG_OK;
        case 1:
            compiler->language = slang::LanguageVersion::v1800_2017;
            return OPTO_SLANG_OK;
        default:
            return fail(compiler, "unsupported slang language version");
    }
}

OptoSlangStatus opto_slang_compiler_set_max_threads(
    OptoSlangCompiler* compiler,
    uint32_t max_threads) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (max_threads == 0) {
        return fail(compiler, "slang max thread count must be positive");
    }
    compiler->max_threads = max_threads;
    return OPTO_SLANG_OK;
}

OptoSlangStatus opto_slang_compiler_compile(
    OptoSlangCompiler* compiler,
    OptoSlangSnapshot** design) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!design) {
        return fail(compiler, "output design pointer is null");
    }
    *design = nullptr;
    if (!has_source_files(*compiler)) {
        return fail(compiler, "slang compile requires at least one input file");
    }

    try {
        auto driver = std::make_unique<Driver>();
        auto compilation = create_compilation(*compiler, *driver);

        const RootSymbol* root = nullptr;
        try {
            root = &compilation->getRoot();
        } catch (const std::exception& err) {
            return fail(compiler, err.what());
        } catch (...) {
            return fail(compiler, "unknown slang elaboration failure");
        }

        std::unordered_set<const InstanceBodySymbol*> visited_bodies;
        for (auto* top : root->topInstances) {
            if (top && visited_bodies.insert(&top->body).second) {
                bind_procedural_bodies(top->body, visited_bodies);
            }
        }

        const Diagnostics* diagnostics = nullptr;
        try {
            diagnostics = &compilation->getAllDiagnostics();
        } catch (const std::exception& err) {
            return fail(compiler, err.what());
        } catch (...) {
            return fail(compiler, "unknown slang diagnostic collection failure");
        }
        if (compilation->hasIssuedErrors() || compilation->hasFatalErrors()) {
            return fail(compiler, diagnostics_to_string(*driver, *diagnostics));
        }

        auto lowered = std::make_unique<OptoSlangSnapshot>();
        lowered->source_manager = &driver->sourceManager;
        opto_slang_prepare_module_names(*lowered, root->topInstances);
        for (auto* top : root->topInstances) {
            if (!top) {
                continue;
            }
            if (lowered->top.empty()) {
                lowered->top = lowered->body_names.at(&top->body);
            }
        }
        opto_slang_collect_modules(
            *lowered,
            root->topInstances,
            std::move(driver),
            std::move(compilation));

        if (lowered->modules.empty()) {
            return fail(compiler, "slang elaboration produced no module instances");
        }

        *design = lowered.release();
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown slang compilation failure");
    }
}

OptoSlangStatus opto_slang_compiler_analyze(
    OptoSlangCompiler* compiler,
    OptoSlangAnalysis** analysis) {
    if (require_compiler(compiler) != OPTO_SLANG_OK) {
        return OPTO_SLANG_ERROR;
    }
    if (!analysis) {
        return fail(compiler, "output analysis pointer is null");
    }
    *analysis = nullptr;
    if (!has_source_files(*compiler)) {
        return fail(compiler, "slang analyze requires at least one input file");
    }

    try {
        Driver driver;
        auto compilation = create_compilation(*compiler, driver);
        auto result = std::make_unique<OptoSlangAnalysis>();
        for (const auto* symbol : compilation->getDefinitions()) {
            if (symbol->kind != SymbolKind::Definition) {
                continue;
            }
            const auto& definition = symbol->as<DefinitionSymbol>();
            if (definition.definitionKind == DefinitionKind::Module) {
                result->definitions.push_back(copy_string(definition.name));
            }
        }
        for (const auto* package : compilation->getPackages()) {
            if (!package->name.empty() && package->name != "std") {
                result->packages.push_back(copy_string(package->name));
            }
        }
        std::ranges::sort(result->definitions);
        result->definitions.erase(
            std::ranges::unique(result->definitions).begin(),
            result->definitions.end());
        std::ranges::sort(result->packages);
        result->packages.erase(
            std::ranges::unique(result->packages).begin(),
            result->packages.end());
        std::map<std::string, std::string> dependencies;
        for (auto buffer : driver.sourceManager.getAllBuffers()) {
            const auto kind = driver.sourceManager.getBufferKind(buffer);
            if (kind != SourceManager::BufferKind::IncludeFile) {
                continue;
            }
            const auto& path = driver.sourceManager.getFullPath(buffer);
            if (path.empty()) {
                continue;
            }
            dependencies.insert_or_assign(
                path.string(),
                copy_string(driver.sourceManager.getSourceText(buffer)));
        }
        for (auto& [path, text] : dependencies) {
            result->dependencies.push_back({std::move(path), std::move(text)});
        }
        if (result->definitions.empty() && result->packages.empty()) {
            return fail(compiler, "slang analysis produced no definitions or packages");
        }
        *analysis = result.release();
        return OPTO_SLANG_OK;
    } catch (const std::exception& err) {
        return fail(compiler, err.what());
    } catch (...) {
        return fail(compiler, "unknown slang analysis failure");
    }
}

const char* opto_slang_compiler_last_error(const OptoSlangCompiler* compiler) {
    if (!compiler) {
        return "native slang compiler is null";
    }
    return compiler->last_error.c_str();
}

void opto_slang_analysis_free(OptoSlangAnalysis* analysis) {
    delete analysis;
}

OptoSlangStatus
opto_slang_analysis_view(const OptoSlangAnalysis* analysis, OptoSlangAnalysisView* view) {
    if (!analysis || !view) {
        return OPTO_SLANG_ERROR;
    }
    *view = OptoSlangAnalysisView{
        analysis->definitions.size(),
        analysis->packages.size(),
        analysis->dependencies.size(),
    };
    return OPTO_SLANG_OK;
}

const char* opto_slang_analysis_definition_name(
    const OptoSlangAnalysis* analysis,
    size_t index) {
    return analysis && index < analysis->definitions.size()
               ? analysis->definitions[index].c_str()
               : nullptr;
}

const char* opto_slang_analysis_package_name(
    const OptoSlangAnalysis* analysis,
    size_t index) {
    return analysis && index < analysis->packages.size() ? analysis->packages[index].c_str()
                                                         : nullptr;
}

OptoSlangStatus opto_slang_analysis_dependency_view(
    const OptoSlangAnalysis* analysis,
    size_t index,
    OptoSlangSourceFileView* view) {
    if (!analysis || !view || index >= analysis->dependencies.size()) {
        return OPTO_SLANG_ERROR;
    }
    const auto& dependency = analysis->dependencies[index];
    *view = OptoSlangSourceFileView{
        dependency.path.c_str(),
        dependency.text ? dependency.text->c_str() : nullptr,
    };
    return OPTO_SLANG_OK;
}

} // extern "C"
