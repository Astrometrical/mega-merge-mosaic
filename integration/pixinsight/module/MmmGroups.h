// MmmGroups.h -- PCL-free helpers for the multi-filter "groups" feature:
// partition panel rows by group name, derive PixInsight-safe window ids,
// and match the Target Frames filter pattern. Header-only so the host CTest
// suite (test_groups) can exercise it without PCL.
#pragma once

#include <cctype>
#include <string>
#include <vector>

namespace mmm_groups
{

struct Group
{
   std::string         name;   // "" = the default group
   std::vector<size_t> rows;   // panel rows, in list order
};

inline std::string trim( const std::string& s )
{
   size_t b = 0, e = s.size();
   while ( b < e && std::isspace( (unsigned char)s[b] ) ) ++b;
   while ( e > b && std::isspace( (unsigned char)s[e - 1] ) ) --e;
   return s.substr( b, e - b );
}

inline std::vector<Group> partition( const std::vector<std::string>& groupOfRow )
{
   std::vector<Group> out;
   for ( size_t row = 0; row < groupOfRow.size(); ++row )
   {
      const std::string& name = groupOfRow[row];
      Group* g = nullptr;
      for ( Group& existing : out )
         if ( existing.name == name )
         {
            g = &existing;
            break;
         }
      if ( g == nullptr )
      {
         out.push_back( Group{ name, {} } );
         g = &out.back();
      }
      g->rows.push_back( row );
   }
   return out;
}

inline std::string sanitize( const std::string& name )
{
   std::string out;
   out.reserve( name.size() );
   for ( unsigned char c : name )
      out += ( std::isalnum( c ) || c == '_' ) ? char( c ) : '_';
   return out;
}

inline std::string window_id( const std::string& group, const std::string& base )
{
   return group.empty() ? base : base + "_" + sanitize( group );
}

inline std::string display_name( const std::string& group )
{
   return group.empty() ? std::string( "(default)" ) : group;
}

inline bool find_window_collision( const std::vector<Group>& groups, const std::string& base,
                                   std::string& a, std::string& b, std::string& id )
{
   for ( size_t i = 0; i < groups.size(); ++i )
      for ( size_t j = i + 1; j < groups.size(); ++j )
         if ( window_id( groups[i].name, base ) == window_id( groups[j].name, base ) )
         {
            a = groups[i].name;
            b = groups[j].name;
            id = window_id( groups[i].name, base );
            return true;
         }
   return false;
}

// Case-insensitive glob: '*' matches any run, '?' one character; an empty
// pattern matches everything (the Target Frames filter's "show all" state).
// Iterative with backtracking to the last '*' (no recursion, linear in
// practice).
inline bool wildcard_match( const std::string& pattern, const std::string& text )
{
   if ( pattern.empty() )
      return true;
   auto lower = []( unsigned char c ) { return char( std::tolower( c ) ); };
   size_t p = 0, t = 0, star = std::string::npos, mark = 0;
   while ( t < text.size() )
   {
      if ( p < pattern.size() && ( pattern[p] == '?' || lower( pattern[p] ) == lower( text[t] ) ) )
      {
         ++p;
         ++t;
      }
      else if ( p < pattern.size() && pattern[p] == '*' )
      {
         star = p++;
         mark = t;
      }
      else if ( star != std::string::npos )
      {
         p = star + 1;
         t = ++mark;
      }
      else
         return false;
   }
   while ( p < pattern.size() && pattern[p] == '*' )
      ++p;
   return p == pattern.size();
}

} // namespace mmm_groups
