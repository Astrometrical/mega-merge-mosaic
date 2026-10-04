// Source-text invariant: the module's interface code never touches the
// PixInsight process console.
//
// pcl::Console's abort-control functions (EnableAbort, AbortRequested,
// Abort, ResetStatus) are valid only on the thread running a process's
// ExecuteGlobal()/ExecuteOn(); from an interface event handler there is no
// processing thread and the core reports "API function error (0x0028):
// Invalid user interface object handle" (seen in the field on Group by
// FILTER, 1.6.0). The module has no runtime test harness, so this reads the
// sources as text:
//   1. MmmInterface.cpp contains no `Console` identifier at all (UI feedback
//      goes through MessageBox);
//   2. the interface-side helper `probe_filter_names` in MmmExecution.cpp
//      does not construct a `ConsoleProgress` (whose idle pump polls
//      Console::AbortRequested()).

#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <string>

namespace {

std::string Slurp( const char* path ) {
  std::ifstream in( path );
  if ( !in ) {
    std::fprintf( stderr, "cannot open %s\n", path );
    std::exit( 1 );
  }
  std::ostringstream ss;
  ss << in.rdbuf();
  return ss.str();
}

// True when `word` occurs in `text` as a whole identifier (not as part of a
// longer identifier such as ConsoleProgress).
bool HasIdentifier( const std::string& text, const std::string& word ) {
  for ( size_t at = 0;; ) {
    const size_t hit = text.find( word, at );
    if ( hit == std::string::npos )
      return false;
    const bool before = hit == 0 || !( std::isalnum( (unsigned char)text[hit - 1] ) || text[hit - 1] == '_' );
    const size_t end = hit + word.size();
    const bool after = end >= text.size() || !( std::isalnum( (unsigned char)text[end] ) || text[end] == '_' );
    if ( before && after )
      return true;
    at = end;
  }
}

// The body of the free function whose definition starts with `signature`:
// from the signature to the first "\n}" that closes it.
std::string FunctionBody( const std::string& text, const std::string& signature ) {
  const size_t start = text.find( signature );
  if ( start == std::string::npos )
    return std::string();
  const size_t end = text.find( "\n}", start );
  return text.substr( start, end == std::string::npos ? std::string::npos : end - start );
}

}  // namespace

int main( int argc, char** argv ) {
  if ( argc != 3 ) {
    std::fprintf( stderr, "usage: %s <MmmInterface.cpp> <MmmExecution.cpp>\n", argv[0] );
    return 2;
  }
  const std::string iface = Slurp( argv[1] );
  const std::string exec = Slurp( argv[2] );
  int failures = 0;

  if ( HasIdentifier( iface, "Console" ) ) {
    std::fprintf( stderr,
                  "FAILED: MmmInterface.cpp uses pcl::Console. Interface event handlers run\n"
                  "        without a processing thread, so Console abort control fails with\n"
                  "        \"Invalid user interface object handle\"; use MessageBox instead.\n" );
    ++failures;
  }

  const std::string body = FunctionBody( exec, "mmm::PanelProbeResult probe_filter_names(" );
  if ( body.empty() ) {
    std::fprintf( stderr, "FAILED: probe_filter_names not found in MmmExecution.cpp\n" );
    ++failures;
  } else if ( HasIdentifier( body, "ConsoleProgress" ) || HasIdentifier( body, "Console" ) ) {
    std::fprintf( stderr,
                  "FAILED: probe_filter_names (interface-side) uses the process console;\n"
                  "        its progress observer must only pump GUI events.\n" );
    ++failures;
  }

  if ( failures == 0 )
    std::fprintf( stderr, "test_interface_no_console: OK\n" );
  return failures == 0 ? 0 : 1;
}
