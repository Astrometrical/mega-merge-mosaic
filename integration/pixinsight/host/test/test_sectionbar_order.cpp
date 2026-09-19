// test_sectionbar_order.cpp -- guards a member-declaration-order invariant in
// MmmBlendInterface::GUIData (module/MmmInterface.h) that, when violated,
// aborts PixInsight the moment the control tree is destroyed.
//
// pcl::SectionBar keeps a raw `Control* m_section` (SectionBar.h:358), set by
// SetSection(). ~SectionBar dereferences it and calls Control::OnShow()/
// OnHide() through the section's PCL handle to detach its own callbacks.
// ~UIObject detaches that handle from the module but never nulls it, so a
// destroyed section still answers with a stale handle: the API call fails,
// pcl::APIFunctionError is thrown, and it escapes the implicitly-noexcept
// ~SectionBar -> std::terminate. C++ destroys members in REVERSE declaration
// order, so a SectionBar declared BEFORE the Control it manages is destroyed
// AFTER it and takes exactly that path.
//
// This shipped in the sibling mpp project (a stack-allocated dialog) and
// aborted PixInsight on every File > Save As. Here GUIData is heap-owned by
// the interface and nothing ever deletes pcl::Module or its children, so the
// destructor is unreachable today -- but this module is a reference for
// future ones, and a stack dialog or an explicit teardown would make it live.
// The rule:
//
//     each section Control must be declared BEFORE the SectionBar that
//     SetSection()s it, so the SectionBar is destroyed first, while the
//     control it points at is still alive.
//
// This test is PCL-free by design -- it reads the two module sources as text,
// which is the only way to check a module invariant in a project whose module
// has no runtime test harness (PCL needs a live core). It pairs each
// `X_SectionBar.SetSection( Y )` in the .cpp with the declaration order of
// X_SectionBar and Y in the .h.

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

// Offset of the declaration of member `name` in the header, or npos.
// Matches the declaration line (type then name), not a use or a comment.
size_t DeclOffset( const std::string& hdr, const std::string& name ) {
  for ( size_t at = 0;; ) {
    const size_t hit = hdr.find( name, at );
    if ( hit == std::string::npos )
      return std::string::npos;
    const size_t eol = hdr.find( '\n', hit );
    const size_t bol = hdr.rfind( '\n', hit );
    std::string line =
        hdr.substr( bol + 1, ( eol == std::string::npos ? hdr.size() : eol ) - bol - 1 );
    line.erase( 0, line.find_first_not_of( " \t" ) );
    // A declaration line ends in ';', has no '(' (which would be a call or a
    // function parameter) and is not a comment.
    if ( line.find( ';' ) != std::string::npos && line.find( '(' ) == std::string::npos &&
         line.compare( 0, 2, "//" ) != 0 )
      return hit;
    at = hit + name.size();
  }
}

}  // namespace

int main( int argc, char** argv ) {
  if ( argc != 3 ) {
    std::fprintf( stderr, "usage: %s <MmmInterface.h> <MmmInterface.cpp>\n", argv[0] );
    return 2;
  }
  const std::string hdr = Slurp( argv[1] );
  const std::string src = Slurp( argv[2] );

  int pairs = 0, failures = 0;
  const std::string call = ".SetSection(";
  for ( size_t at = 0;; ) {
    const size_t hit = src.find( call, at );
    if ( hit == std::string::npos )
      break;
    at = hit + call.size();

    // Left of ".SetSection(" is the bar's name; inside the parens, the section.
    const size_t barEnd = hit;
    size_t barBeg = barEnd;
    while ( barBeg > 0 && ( std::isalnum( (unsigned char)src[barBeg - 1] ) || src[barBeg - 1] == '_' ) )
      --barBeg;
    const std::string bar = src.substr( barBeg, barEnd - barBeg );

    const size_t close = src.find( ')', at );
    std::string section = src.substr( at, close - at );
    section.erase( 0, section.find_first_not_of( " \t" ) );
    section.erase( section.find_last_not_of( " \t" ) + 1 );

    const size_t barDecl = DeclOffset( hdr, bar );
    const size_t secDecl = DeclOffset( hdr, section );
    if ( barDecl == std::string::npos || secDecl == std::string::npos ) {
      std::fprintf( stderr, "FAILED: cannot locate declarations for %s / %s\n",
                    bar.c_str(), section.c_str() );
      ++failures;
      continue;
    }

    ++pairs;
    if ( secDecl > barDecl ) {
      std::fprintf( stderr,
                    "FAILED: %s is declared before its section %s.\n"
                    "        Members are destroyed in reverse declaration order, so %s\n"
                    "        would be destroyed AFTER %s and call OnShow()/OnHide() on a\n"
                    "        destroyed control in ~SectionBar -- pcl::APIFunctionError\n"
                    "        escaping a noexcept destructor, i.e. std::terminate.\n"
                    "        Declare %s BEFORE %s.\n",
                    bar.c_str(), section.c_str(), bar.c_str(), section.c_str(),
                    section.c_str(), bar.c_str() );
      ++failures;
    }
  }

  if ( pairs == 0 ) {
    std::fprintf( stderr, "FAILED: no SetSection() pairs found -- the test parsed nothing\n" );
    return 1;
  }
  std::printf( "checked %d SectionBar/section pair(s), %d failure(s)\n", pairs, failures );
  return failures == 0 ? 0 : 1;
}
